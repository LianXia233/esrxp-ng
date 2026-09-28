//! axum HTTP API —— 对接 TDesign/Electron UI。
//!
//! 契约与 Python 版 server.py 一致：
//!   GET  /api/info
//!   POST /api/video/open {path}
//!   POST /api/preview  {path, frame, config, region_only}
//!   POST /api/rip      {path, out_dir, config, auto_color}  → 后台任务
//!   GET  /api/jobs/{id}
//!   GET  /api/artifact?path=...
//! 静态：/vendor、/ui（TDesign 前端）

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::ServeDir;

use crate::config::AppConfig;
use crate::outputs::{write_json_timeline, write_ocr_png, write_project, write_ssa, write_vobsub};
use crate::ripper::rip;
use crate::video::VideoSource;

pub type Jobs = Arc<Mutex<HashMap<String, Job>>>;

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub status: String,
    pub video: String,
    pub out_dir: String,
    pub progress: Value,
    pub log: Vec<String>,
    pub result: Option<Value>,
    pub started: f64,
}

#[derive(Deserialize)]
pub struct OpenReq {
    pub path: String,
}

#[derive(Deserialize)]
pub struct PreviewReq {
    pub path: String,
    pub frame: i64,
    pub config: Value,
    pub region_only: Option<bool>,
}

#[derive(Deserialize)]
pub struct RipReq {
    pub path: String,
    pub out_dir: String,
    pub config: Value,
    pub auto_color: Option<bool>,
}

pub fn router(ui_dir: String, cache_dir: String) -> Router {
    let state: AppState = AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        ui_dir,
        cache_dir,
    };
    let ui_dir2 = state.ui_dir.clone();
    let cors = tower_http::cors::CorsLayer::permissive();
    Router::new()
        .route("/api/info", get(api_info))
        .route("/api/video/open", post(api_video_open))
        .route("/api/preview", post(api_preview))
        .route("/api/rip", post(api_rip))
        .route("/api/jobs/{id}", get(api_job))
        .route("/api/artifact", get(api_artifact))
        .fallback_service(ServeDir::new(&ui_dir2))
        .layer(cors)
        .with_state(state)
}

#[derive(Clone)]
pub struct AppState {
    pub jobs: Jobs,
    pub ui_dir: String,
    pub cache_dir: String,
}

fn err_response(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response()
}

async fn api_info() -> Json<Value> {
    Json(json!({"name": "esrxp-ng-server", "version": env!("CARGO_PKG_VERSION")}))
}

async fn api_video_open(Json(req): Json<OpenReq>) -> Response {
    if !Path::new(&req.path).is_file() {
        return err_response(&format!("文件不存在: {}", req.path));
    }
    match VideoSource::open(&req.path) {
        Ok(vs) => Json(json!({
            "path": vs.path, "width": vs.width, "height": vs.height,
            "fps": vs.fps, "duration_s": vs.duration, "frame_count": vs.frame_count,
            "codec": vs.codec_name, "pix_fmt": vs.pix_fmt,
        }))
        .into_response(),
        Err(e) => err_response(&format!("无法打开视频: {e}")),
    }
}

async fn api_preview(State(st): State<AppState>, Json(req): Json<PreviewReq>) -> Response {
    if !Path::new(&req.path).is_file() {
        return err_response(&format!("文件不存在: {}", req.path));
    }
    let cache = st.cache_dir.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let cfg: AppConfig = serde_json::from_value(req.config).unwrap_or_default();
        let mut vs = VideoSource::open(&req.path).map_err(|e| e.to_string())?;
        let frame_no = req.frame.max(0);
        let mut fd_opt: Option<crate::video::FrameData> = None;
        vs.decode_range(frame_no, frame_no + 1, 1, |fd| {
            fd_opt = Some(fd);
            Ok(())
        }).map_err(|e| e.to_string())?;
        let fd = fd_opt.ok_or("帧不存在")?;
        let (roi, rw, rh, origin) = crate::ripper::prepare_roi(&fd, &cfg);
        let mask = crate::postprocess::clean(
            &crate::gpu::kernels().filter(&roi, rw, rh, &cfg.filter), rw, rh, &cfg.postprocess);
        let (base, m, mw, mh, ox, oy) = if req.region_only.unwrap_or(false) {
            (roi, mask, rw, rh, 0i64, 0i64)
        } else {
            let mut full_mask = vec![0u8; fd.width * fd.height];
            for y in 0..rh {
                for x in 0..rw {
                    if mask[y * rw + x] > 0 {
                        full_mask[(origin.1 as usize + y) * fd.width + origin.0 as usize + x] = 255;
                    }
                }
            }
            (fd.rgb.clone(), full_mask, fd.width, fd.height, 0i64, 0i64)
        };
        let _ = (ox, oy);
        // 拼接：左原图 | 中 mask | 右叠加
        let pw = mw + mw + mw;
        let ph = mh;
        let mut img = image::RgbaImage::new(pw as u32, ph as u32);
        for y in 0..ph {
            for x in 0..mw {
                let src = y * mw + x;
                let (r, g, b) = if m[src] > 0 {
                    if x < mw {
                        // 中：mask 白底
                        (255u8, 255u8, 255u8)
                    } else {
                        (0, 0, 0)
                    }
                } else {
                    (0, 0, 0)
                };
                let _ = (r, g, b);
                // 左原图
                let ps = src * 3;
                img.put_pixel(x as u32, y as u32, image::Rgba([base[ps], base[ps + 1], base[ps + 2], 255]));
            }
        }
        // 简化拼接：改为三列直接写
        for y in 0..ph {
            for x in 0..mw {
                let src = y * mw + x;
                let ps = src * 3;
                let (r, g, b) = (base[ps], base[ps + 1], base[ps + 2]);
                img.put_pixel(x as u32, y as u32, image::Rgba([r, g, b, 255]));
                // 中列：mask（白=命中）
                let mv = if m[src] > 0 { 255u8 } else { 0u8 };
                img.put_pixel((mw + x) as u32, y as u32, image::Rgba([mv, mv, mv, 255]));
                // 右列：叠加（命中保留原色，未命中绿色调）
                let (r2, g2, b2) = if m[src] > 0 {
                    (r, g, b)
                } else {
                    (r.saturating_mul(2).min(255) / 2 + 30, (g as u16 + 90).min(255) as u8, b.saturating_mul(2).min(255) / 2)
                };
                img.put_pixel((mw * 2 + x) as u32, y as u32, image::Rgba([r2, g2.clamp(0, 255), b2, 255]));
            }
        }
        let p = Path::new(&cache).join(format!("preview_{}.png", Instant::now().elapsed().as_micros() % 1_000_000));
        img.save(&p).map_err(|e| e.to_string())?;
        Ok(json!({"image": format!("/api/artifact?path={}", p.display()), "frame": fd.index, "time": fd.time}))
    })
    .await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err_response(&e),
        Err(e) => err_response(&e.to_string()),
    }
}

async fn api_rip(State(st): State<AppState>, Json(req): Json<RipReq>) -> Response {
    if !Path::new(&req.path).is_file() {
        return err_response(&format!("文件不存在: {}", req.path));
    }
    let cfg: AppConfig = serde_json::from_value(req.config).unwrap_or_default();
    let id = format!("{:012x}", rand_id());
    let job = Job {
        id: id.clone(),
        status: "running".into(),
        video: req.path.clone(),
        out_dir: req.out_dir.clone(),
        progress: json!({"done": 0, "total": 0, "found": 0, "pct": 0}),
        log: vec![],
        result: None,
        started: Instant::now().elapsed().as_secs_f64(),
    };
    {
        let mut jobs = st.jobs.lock().unwrap();
        jobs.insert(id.clone(), job);
    }
    let jobs = st.jobs.clone();
    let id2 = id.clone();
    let out_dir_owned = req.out_dir.clone();
    std::thread::spawn(move || {
        let mut log: Vec<String> = Vec::new();
        let mut result: Option<Value> = None;
        let mut status = "running";
        let out_dir = Path::new(&out_dir_owned);
        match (|| -> Result<(), String> {
            std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
            let mut vs = VideoSource::open(&req.path).map_err(|e| e.to_string())?;
            log.push(format!("视频: {}x{} @ {:.2} fps, {:.2}s",
                             vs.width, vs.height, vs.fps, vs.duration));
            let progress_jobs = jobs.clone();
            let progress_id = id2.clone();
            let res = rip(&mut vs, &cfg, Some(move |done, total, found| {
                let pct = if total > 0 { done * 100 / total } else { 0 };
                if let Ok(mut g) = progress_jobs.lock() {
                    if let Some(j) = g.get_mut(&progress_id) {
                        j.progress = json!({"done": done, "total": total, "found": found, "pct": pct});
                    }
                }
            })).map_err(|e| e.to_string())?;

            let mut artifacts: HashMap<String, String> = HashMap::new();
            let src = Path::new(&req.path).file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "subtitle".into());
            let vinfo = res.video_info.clone();
            if !res.events.is_empty() && cfg.output.ssa {
                let p = out_dir.join(format!("{src}.ass"));
                write_ssa(&res.events, vinfo["width"].as_u64().unwrap_or(0) as usize,
                          vinfo["height"].as_u64().unwrap_or(0) as usize, &cfg, &p)
                    .map_err(|e| e.to_string())?;
                artifacts.insert("ssa".into(), p.display().to_string());
            }
            if !res.events.is_empty() && cfg.output.vobsub {
                let (sp, ip) = write_vobsub(&res.events,
                    vinfo["width"].as_u64().unwrap_or(0) as usize,
                    vinfo["height"].as_u64().unwrap_or(0) as usize, &cfg, &out_dir.join(&src))
                    .map_err(|e| e.to_string())?;
                artifacts.insert("sub".into(), sp);
                artifacts.insert("idx".into(), ip);
            }
            if !res.events.is_empty() && cfg.output.ocr_png {
                let files = write_ocr_png(&res.events, &cfg, &out_dir.join("subtitle_imgs"))
                    .map_err(|e| e.to_string())?;
                artifacts.insert("ocr_images".into(), out_dir.join("subtitle_imgs").display().to_string());
                let _ = files;
            }
            if cfg.output.json_timeline {
                let p = out_dir.join(format!("{src}.timeline.json"));
                write_json_timeline(&res.events, &vinfo, &cfg, &p).map_err(|e| e.to_string())?;
                artifacts.insert("timeline".into(), p.display().to_string());
            }
            let proj = out_dir.join(format!("{src}.esrng.json"));
            write_project(&res.events, &req.path, &cfg, &proj, &json!(artifacts))
                .map_err(|e| e.to_string())?;
            artifacts.insert("project".into(), proj.display().to_string());

            let events: Vec<Value> = res.events.iter().enumerate().map(|(i, e)| {
                json!({
                    "index": i + 1, "start": e.start, "end": e.end,
                    "start_frame": e.start_frame, "end_frame": e.end_frame,
                    "bbox": e.bbox, "duration": (e.end - e.start) * 100.0f64.round() / 100.0,
                    "diff_frames": e.diff_frames,
                })
            }).collect();
            result = Some(json!({
                "events": events, "artifacts": artifacts, "video_info": vinfo,
                "frames_processed": res.frames_processed, "candidates": res.candidates,
                "elapsed_s": (res.elapsed_s * 100.0).round() / 100.0,
            }));
            log.push(format!("完成：字幕 {} 条，耗时 {:.2}s", res.events.len(), res.elapsed_s));
            status = "done";
            Ok(())
        })() {
            Ok(()) => {}
            Err(e) => {
                log.push(format!("错误: {e}"));
                status = "error";
            }
        }
        if let Ok(mut g) = jobs.lock() {
            if let Some(j) = g.get_mut(&id2) {
                j.status = status.to_string();
                j.log = log;
                j.result = result;
            }
        }
    });
    Json(json!({"job_id": id})).into_response()
}

async fn api_job(State(st): State<AppState>, AxPath(job_id): AxPath<String>) -> Response {
    let jobs = st.jobs.lock().unwrap();
    match jobs.get(&job_id) {
        Some(j) => Json(json!({
            "id": j.id, "status": j.status, "video": j.video, "out_dir": j.out_dir,
            "progress": j.progress, "log": j.log, "result": j.result, "started": j.started,
        }))
        .into_response(),
        None => err_response("任务不存在"),
    }
}

async fn api_artifact(query: axum::extract::Query<HashMap<String, String>>) -> Response {
    match query.get("path") {
        Some(p) => {
            let path = Path::new(p);
            if path.is_file() {
                let data = std::fs::read(path).unwrap_or_default();
                let ct = mime_guess_light(path);
                axum::response::Response::builder()
                    .header(axum::http::header::CONTENT_TYPE, ct)
                    .body(axum::body::Body::from(data))
                    .unwrap_or_else(|_| err_response("响应构造失败"))
            } else {
                err_response("文件不存在")
            }
        }
        None => err_response("缺少 path 参数"),
    }
}

fn mime_guess_light(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("ass") => "text/plain; charset=utf-8",
        Some("sub") => "application/octet-stream",
        Some("idx") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn rand_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() as u64;
    n ^ (std::process::id() as u64) << 32
}
