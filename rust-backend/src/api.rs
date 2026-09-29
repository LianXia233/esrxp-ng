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
use crate::outputs::{load_esr, write_esr, write_json_timeline, write_ocr_png, write_srt, write_srt_bitmap, write_ssa, write_vobsub};
use crate::ripper::{crop_event, rip, SubtitleEvent};
use crate::video::VideoSource;

pub type Jobs = Arc<Mutex<HashMap<String, Job>>>;
pub type Batches = Arc<Mutex<HashMap<String, BatchJob>>>;

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

#[derive(Deserialize)]
pub struct BatchReq {
    pub files: Vec<String>,
    pub out_dir: String,
    pub config: Value,
}

#[derive(Deserialize)]
pub struct PixelReq {
    pub path: String,
    pub frame: i64,
    pub x: i64,
    pub y: i64,
}

#[derive(Deserialize)]
pub struct ManagerOp {
    pub project: String,        // .esr 工程路径
    pub indexes: Option<Vec<usize>>,
}

#[derive(Deserialize)]
pub struct ProjectOpenReq {
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct BatchSub {
    pub id: String,
    pub file: String,
    pub status: String,
    pub progress: Value,
    pub result: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct BatchJob {
    pub id: String,
    pub files: Vec<String>,
    pub out_dir: String,
    pub status: String,
    pub subs: Vec<BatchSub>,
}

pub fn router(ui_dir: String, cache_dir: String) -> Router {
    let state: AppState = AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        batches: Arc::new(Mutex::new(HashMap::new())),
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
        .route("/api/batch", post(api_batch))
        .route("/api/batch/{id}", get(api_batch_job))
        .route("/api/pixel", post(api_pixel))
        .route("/api/manager/list", post(api_manager_list))
        .route("/api/manager/recover", post(api_manager_recover))
        .route("/api/manager/remove", post(api_manager_remove))
        .route("/api/manager/purge", post(api_manager_purge))
        .route("/api/manager/crop", post(api_manager_crop))
        .route("/api/manager/export", post(api_manager_export))
        .route("/api/project/open", post(api_project_open))
        .route("/api/artifact", get(api_artifact))
        .fallback_service(ServeDir::new(&ui_dir2))
        .layer(cors)
        .with_state(state)
}

#[derive(Clone)]
pub struct AppState {
    pub jobs: Jobs,
    pub batches: Batches,
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

            let (mut artifacts, project_path) = write_all_outputs(
                &res.events, &res.filtered, &req.path, &cfg, out_dir)?;
            let vinfo = res.video_info.clone();

            let events: Vec<Value> = res.events.iter().enumerate().map(|(i, e)| {
                json!({
                    "index": i + 1, "start": e.start, "end": e.end,
                    "start_frame": e.start_frame, "end_frame": e.end_frame,
                    "bbox": e.bbox, "duration": (e.end - e.start) * 100.0f64.round() / 100.0,
                    "diff_frames": e.diff_frames, "deleted": e.deleted,
                })
            }).collect();
            let filtered_meta: Vec<Value> = res.filtered.iter().enumerate().map(|(i, e)| {
                json!({
                    "index": i + 1, "start": e.start, "end": e.end,
                    "start_frame": e.start_frame, "end_frame": e.end_frame,
                    "bbox": e.bbox,
                })
            }).collect();
            result = Some(json!({
                "events": events, "filtered": filtered_meta,
                "artifacts": artifacts, "project": project_path,
                "video_info": vinfo,
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

// ------------------------------------------------------------------ 共享产物写出
/// 根据当前 events/filtered + 配置，把所有输出（SSA/VobSub/OCR/SRT/SRT+bitmap/JSON/.esr）
/// 写到 out_dir。返回 (artifacts, project_path)。
pub fn write_all_outputs(events: &[SubtitleEvent], filtered: &[SubtitleEvent],
                     video_path: &str, cfg: &AppConfig, out_dir: &Path)
                     -> Result<(HashMap<String, String>, String), String> {
    let mut artifacts: HashMap<String, String> = HashMap::new();
    let src = Path::new(video_path).file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "subtitle".into());
    let vinfo = json!({"width": 0, "height": 0});
    let (vw, vh) = {
        match VideoSource::open(video_path) {
            Ok(v) => (v.width, v.height),
            Err(_) => (0, 0),
        }
    };
    let _ = vinfo;
    if !events.is_empty() && cfg.output.ssa {
        let p = out_dir.join(format!("{src}.ass"));
        write_ssa(events, vw, vh, cfg, &p).map_err(|e| e.to_string())?;
        artifacts.insert("ssa".into(), p.display().to_string());
    }
    if !events.is_empty() && cfg.output.vobsub {
        let (sp, ip) = write_vobsub(events, vw, vh, cfg, &out_dir.join(&src))
            .map_err(|e| e.to_string())?;
        artifacts.insert("sub".into(), sp);
        artifacts.insert("idx".into(), ip);
    }
    if !events.is_empty() && cfg.output.ocr_png {
        let files = write_ocr_png(events, cfg, &out_dir.join("subtitle_imgs"))
            .map_err(|e| e.to_string())?;
        artifacts.insert("ocr_images".into(), out_dir.join("subtitle_imgs").display().to_string());
        let _ = files;
    }
    if !events.is_empty() && cfg.output.srt {
        let p = out_dir.join(format!("{src}.srt"));
        write_srt(events, cfg, &p).map_err(|e| e.to_string())?;
        artifacts.insert("srt".into(), p.display().to_string());
        if cfg.output.srt_bitmap {
            let (dir, files) = write_srt_bitmap(events, cfg, &p).map_err(|e| e.to_string())?;
            artifacts.insert("srt_subs".into(), dir);
            let _ = files;
        }
    }
    if cfg.output.json_timeline {
        let p = out_dir.join(format!("{src}.timeline.json"));
        write_json_timeline(events, &json!({"width": vw, "height": vh}), cfg, &p)
            .map_err(|e| e.to_string())?;
        artifacts.insert("timeline".into(), p.display().to_string());
    }
    let proj = out_dir.join(format!("{src}.esr"));
    write_esr(events, filtered, video_path, cfg, &proj, &json!(artifacts))
        .map_err(|e| e.to_string())?;
    artifacts.insert("project".into(), proj.display().to_string());
    Ok((artifacts, proj.display().to_string()))
}

// ------------------------------------------------------------------ 批处理
async fn api_batch(State(st): State<AppState>, Json(req): Json<BatchReq>) -> Response {
    let files: Vec<String> = req.files.iter().filter(|p| Path::new(p).is_file()).cloned().collect();
    if files.is_empty() {
        return err_response("没有有效的视频文件");
    }
    let id = format!("{:012x}", rand_id());
    let subs: Vec<BatchSub> = files.iter().map(|f| BatchSub {
        id: format!("{:012x}", rand_id()),
        file: f.clone(), status: "pending".into(),
        progress: json!({"done": 0, "total": 0, "found": 0, "pct": 0}),
        result: None,
    }).collect();
    let batch = BatchJob {
        id: id.clone(), files, out_dir: req.out_dir.clone(),
        status: "running".into(), subs,
    };
    {
        let mut g = st.batches.lock().unwrap();
        g.insert(id.clone(), batch);
    }
    let batches = st.batches.clone();
    let cfg: AppConfig = serde_json::from_value(req.config).unwrap_or_default();
    let out_dir = req.out_dir.clone();
    let bid = id.clone();
    std::thread::spawn(move || {
        let mut done_cnt = 0usize;
        loop {
            // 取出下一个 pending 子任务
            let next = {
                let g = batches.lock().unwrap();
                let b = g.get(&bid);
                let mut pick: Option<(String, String)> = None;
                if let Some(b) = b {
                    for s in &b.subs {
                        if s.status == "pending" {
                            pick = Some((s.id.clone(), s.file.clone()));
                            break;
                        }
                    }
                }
                pick
            };
            let (sub_id, file) = match next {
                Some(x) => x,
                None => break,
            };
            {
                let mut g = batches.lock().unwrap();
                if let Some(b) = g.get_mut(&bid) {
                    for s in &mut b.subs {
                        if s.id == sub_id { s.status = "running".to_string(); }
                    }
                }
            }
            let (bid2, sub_id2, batches2) = (bid.clone(), sub_id.clone(), batches.clone());
            let res = run_one_rip(&file, &out_dir, &cfg, move |done, total, found| {
                let pct = if total > 0 { done * 100 / total } else { 0 };
                let mut g = batches2.lock().unwrap();
                if let Some(b) = g.get_mut(&bid2) {
                    for s in &mut b.subs {
                        if s.id == sub_id2 {
                            s.progress = json!({"done": done, "total": total, "found": found, "pct": pct});
                        }
                    }
                }
            });
            {
                let mut g = batches.lock().unwrap();
                if let Some(b) = g.get_mut(&bid) {
                    for s in &mut b.subs {
                        if s.id == sub_id {
                            match &res {
                                Ok(r) => { s.status = "done".to_string(); s.result = Some(r.clone()); }
                                Err(e) => { s.status = "error".to_string(); s.result = Some(json!({"error": e})); }
                            }
                        }
                    }
                    done_cnt += 1;
                    if done_cnt >= b.subs.len() {
                        b.status = "done".to_string();
                    }
                }
            }
        }
    });
    Json(json!({"batch_id": id})).into_response()
}

async fn api_batch_job(State(st): State<AppState>, AxPath(bid): AxPath<String>) -> Response {
    let g = st.batches.lock().unwrap();
    match g.get(&bid) {
        Some(b) => {
            let subs: Vec<Value> = b.subs.iter().map(|s| json!({
                "id": s.id, "file": s.file, "status": s.status,
                "progress": s.progress, "result": s.result,
            })).collect();
            Json(json!({"id": b.id, "status": b.status, "out_dir": b.out_dir, "subs": subs})).into_response()
        }
        None => err_response("批处理任务不存在"),
    }
}

/// 单文件 rip（批处理子任务复用）。
fn run_one_rip(path: &str, out_dir: &str, cfg: &AppConfig,
               on_progress: impl Fn(i64, i64, i64) + Send + Sync + 'static)
               -> Result<Value, String> {
    let out_dir = Path::new(out_dir);
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let mut vs = VideoSource::open(path).map_err(|e| e.to_string())?;
    let res = rip(&mut vs, cfg, Some(on_progress)).map_err(|e| e.to_string())?;
    let (artifacts, project_path) = write_all_outputs(&res.events, &res.filtered, path, cfg, out_dir)?;
    let events: Vec<Value> = res.events.iter().enumerate().map(|(i, e)| json!({
        "index": i + 1, "start": e.start, "end": e.end,
        "start_frame": e.start_frame, "end_frame": e.end_frame,
        "bbox": e.bbox, "diff_frames": e.diff_frames, "deleted": e.deleted,
    })).collect();
    Ok(json!({
        "events": events, "artifacts": artifacts, "project": project_path,
        "frames_processed": res.frames_processed, "candidates": res.candidates,
        "elapsed_s": (res.elapsed_s * 100.0).round() / 100.0,
    }))
}

// ------------------------------------------------------------------ Pixel Color 取色
async fn api_pixel(Json(req): Json<PixelReq>) -> Response {
    if !Path::new(&req.path).is_file() {
        return err_response("文件不存在");
    }
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mut vs = VideoSource::open(&req.path).map_err(|e| e.to_string())?;
        let mut fd_opt = None;
        let f = req.frame.max(0);
        vs.decode_range(f, f + 1, 1, |fd| { fd_opt = Some(fd); Ok(()) })
            .map_err(|e| e.to_string())?;
        let fd = fd_opt.ok_or("帧不存在")?;
        let (x, y) = (req.x.clamp(0, fd.width as i64 - 1) as usize,
                      req.y.clamp(0, fd.height as i64 - 1) as usize);
        let ps = (y * fd.width + x) * 3;
        Ok(json!({"rgb": [fd.rgb[ps], fd.rgb[ps + 1], fd.rgb[ps + 2]],
                  "x": x, "y": y, "frame": fd.index}))
    }).await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err_response(&e),
        Err(e) => err_response(&e.to_string()),
    }
}

// ------------------------------------------------------------------ 字幕管理器
fn load_project_for_manage(project: &str) -> Result<(Vec<SubtitleEvent>, Vec<SubtitleEvent>, String, AppConfig, String), Response> {
    let p = Path::new(project);
    if !p.is_file() {
        return Err(err_response(&format!("工程文件不存在: {project}")));
    }
    match load_esr(p) {
        Ok((events, filtered, video, cfg)) => {
            let out_dir = p.parent().map(|d| d.display().to_string()).unwrap_or_else(|| ".".into());
            Ok((events, filtered, video, cfg, out_dir))
        }
        Err(e) => Err(err_response(&format!("工程文件解析失败: {e}"))),
    }
}

fn events_meta(events: &[SubtitleEvent]) -> Vec<Value> {
    events.iter().enumerate().map(|(i, e)| json!({
        "index": i + 1, "start": (e.start * 100.0).round() / 100.0,
        "end": (e.end * 100.0).round() / 100.0,
        "start_frame": e.start_frame, "end_frame": e.end_frame,
        "bbox": e.bbox, "diff_frames": e.diff_frames, "deleted": e.deleted,
    })).collect()
}

async fn api_manager_list(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (events, filtered, _video, _cfg, _out) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    Json(json!({
        "subtitles": events_meta(&events),
        "filtered": events_meta(&filtered),
        "count": events.len(), "filtered_count": filtered.len(),
    })).into_response()
}

async fn api_manager_recover(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (mut events, mut filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    // Recover Filtered：把被过滤候选恢复到字幕列表
    for f in filtered.drain(..) {
        let mut e = f;
        e.deleted = false;
        events.push(e);
    }
    events.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    let (artifacts, proj) = match write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir)) {
        Ok(v) => v,
        Err(e) => return err_response(&e),
    };
    let _ = proj;
    Json(json!({"count": events.len(), "artifacts": artifacts, "subtitles": events_meta(&events)})).into_response()
}

async fn api_manager_remove(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (mut events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let idxs = req.indexes.unwrap_or_default();
    for i in &idxs {
        if let Some(e) = events.get_mut(*i) {
            e.deleted = true;
        }
    }
    let (artifacts, proj) = match write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir)) {
        Ok(v) => v,
        Err(e) => return err_response(&e),
    };
    let _ = proj;
    Json(json!({"count": events.len(), "artifacts": artifacts, "subtitles": events_meta(&events)})).into_response()
}

async fn api_manager_purge(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let kept: Vec<SubtitleEvent> = events.into_iter().filter(|e| !e.deleted).collect();
    let (artifacts, proj) = match write_all_outputs(&kept, &filtered, &video, &cfg, Path::new(&out_dir)) {
        Ok(v) => v,
        Err(e) => return err_response(&e),
    };
    let _ = proj;
    Json(json!({"count": kept.len(), "artifacts": artifacts, "subtitles": events_meta(&kept)})).into_response()
}

async fn api_manager_crop(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (mut events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let idxs = req.indexes.unwrap_or_default();
    if idxs.is_empty() {
        return err_response("请指定要裁剪的字幕序号");
    }
    let res = tokio::task::spawn_blocking(move || -> Result<Vec<SubtitleEvent>, String> {
        let mut vs = VideoSource::open(&video).map_err(|e| e.to_string())?;
        let mut updated = Vec::new();
        for i in &idxs {
            if let Some(ev) = events.get(*i).cloned() {
                let c = crop_event(&mut vs, &cfg, &ev).map_err(|e| e.to_string())?;
                updated.push((*i, c));
            }
        }
        for (i, c) in updated {
            events[i] = c;
        }
        let (artifacts, proj) = write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir))
            .map_err(|e| e.to_string())?;
        let _ = proj;
        Ok(events)
    }).await;
    match res {
        Ok(Ok(events)) => Json(json!({"count": events.len(), "subtitles": events_meta(&events)})).into_response(),
        Ok(Err(e)) => err_response(&e),
        Err(e) => err_response(&e.to_string()),
    }
}

async fn api_manager_export(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let (artifacts, _proj) = match write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir)) {
        Ok(v) => v,
        Err(e) => return err_response(&e),
    };
    Json(json!({"artifacts": artifacts, "count": events.len()})).into_response()
}

// ------------------------------------------------------------------ 打开工程
async fn api_project_open(Json(req): Json<ProjectOpenReq>) -> Response {
    let r = load_project_for_manage(&req.path);
    let (events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    Json(json!({
        "video": video, "out_dir": out_dir,
        "config": serde_json::to_value(&cfg).unwrap_or(json!({})),
        "subtitles": events_meta(&events), "filtered": events_meta(&filtered),
        "count": events.len(), "filtered_count": filtered.len(),
    })).into_response()
}

fn rand_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() as u64;
    n ^ (std::process::id() as u64) << 32
}
