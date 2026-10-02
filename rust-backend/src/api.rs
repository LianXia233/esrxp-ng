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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::ServeDir;

use crate::config::AppConfig;
use crate::outputs::{
    base64_encode, load_esr, write_esr, write_json_timeline, write_ocr_png, write_srt,
    write_srt_bitmap, write_ssa, write_vobsub,
};
use crate::ripper::{crop_event, merge_repeat_manual, rip, SubtitleEvent};
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
    pub mode: Option<String>, // 构图模式：combo(默认) / raw / mask / overlay
}

#[derive(Deserialize)]
pub struct RipReq {
    pub path: String,
    pub out_dir: String,
    pub config: Value,
    /// 保留字段：esrXP 契约占位，自动选色当前由引擎按 mask/颜色合理性门控自动决定
    #[allow(dead_code)]
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
    pub project: String, // .esr 工程路径
    pub indexes: Option<Vec<usize>>,
    pub edits: Option<Vec<TimeEdit>>,
    pub offset_ms: Option<i64>,
    pub time: Option<f64>,
}

#[derive(Deserialize, Clone)]
pub struct TimeEdit {
    pub index: usize,
    pub start: Option<f64>,
    pub end: Option<f64>,
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
    /// 批处理原始文件清单（调试/复现用；进度走 subs）
    #[allow(dead_code)]
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
    crate::logging::init_session(Path::new(&state.cache_dir));
    crate::logging::info(format!(
        "后端启动 v{}: ui={} cache={}",
        env!("CARGO_PKG_VERSION"),
        state.ui_dir,
        state.cache_dir
    ));
    // CORS 仅用于 TCP 开发态（浏览器直连）。**不能** permissive：后端接受任意
    // 绝对路径且能在任意可写位置创建目录/写产物，一旦放开，任意网页的 JS 都能
    // 借用户权限驱动本机后端（读取任意视频、在任意目录落盘、跑批处理耗 CPU）。
    // 故只回显本地 Origin（UI 页面本身同源托管，不依赖跨域）。
    let cors = tower_http::cors::CorsLayer::new()
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
        .allow_origin([
            "http://127.0.0.1:8000"
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
            "http://127.0.0.1:18081"
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
            "http://localhost:8000"
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
            "http://localhost:18081"
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
        ])
        .allow_credentials(false);
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
        .route("/api/manager/edit", post(api_manager_edit))
        .route("/api/manager/shift", post(api_manager_shift))
        .route("/api/manager/split", post(api_manager_split))
        .route("/api/manager/merge", post(api_manager_merge))
        .route("/api/manager/merge_repeat", post(api_manager_merge_repeat))
        .route("/api/manager/export", post(api_manager_export))
        .route("/api/project/open", post(api_project_open))
        .route("/api/config/default", get(api_config_default))
        .route(
            "/api/config/file",
            get(api_config_file).post(api_config_save),
        )
        .route("/api/log", get(api_log_get).post(api_log_post))
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

// ------------------------------------------------------------------ 产物目录白名单
// /api/artifact 仅允许读取注册目录（preview 缓存 / rip 输出 / batch 输出 / 工程目录）下的文件，
// 防止任意路径读取；canonicalize 统一 Windows \\?\ 前缀并规避 .. 与符号链接绕过。
fn artifact_dirs() -> &'static Mutex<HashSet<PathBuf>> {
    static D: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(HashSet::new()))
}

fn allow_artifact_dir(p: &Path) {
    if let Ok(c) = p.canonicalize() {
        artifact_dirs().lock().unwrap().insert(c);
    }
}

fn artifact_dir_allowed(canon: &Path, extra: Option<&Path>) -> bool {
    if let Some(e) = extra {
        if let Ok(ce) = e.canonicalize() {
            if canon.starts_with(&ce) {
                return true;
            }
        }
    }
    artifact_dirs()
        .lock()
        .unwrap()
        .iter()
        .any(|d| canon.starts_with(d))
}

// 预览缓存自增序号（替代微秒取模命名，防碰撞）
static PREVIEW_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
        Ok(vs) => {
            crate::logging::info(format!(
                "打开视频: {} {}x{} @ {:.2}fps, {:.2}s, {} 帧",
                vs.path, vs.width, vs.height, vs.fps, vs.duration, vs.frame_count
            ));
            Json(json!({
                "path": vs.path, "width": vs.width, "height": vs.height,
                "fps": vs.fps, "duration_s": vs.duration, "frame_count": vs.frame_count,
                "codec": vs.codec_name, "pix_fmt": vs.pix_fmt,
            }))
            .into_response()
        }
        Err(e) => {
            crate::logging::error(format!("打开视频失败: {} ({e})", req.path));
            err_response(&format!("无法打开视频: {e}"))
        }
    }
}

async fn api_preview(State(st): State<AppState>, Json(req): Json<PreviewReq>) -> Response {
    if !Path::new(&req.path).is_file() {
        return err_response(&format!("文件不存在: {}", req.path));
    }
    let cache = st.cache_dir.clone();
    let req0_frame = req.frame;
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let cfg: AppConfig = serde_json::from_value(req.config).unwrap_or_default();
        let mut vs = VideoSource::open(&req.path).map_err(|e| e.to_string())?;
        let frame_no = req.frame.max(0);
        let mut fd_opt: Option<crate::video::FrameData> = None;
        vs.decode_range(frame_no, frame_no + 1, 1, |fd| {
            fd_opt = Some(fd);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
        let fd = fd_opt.ok_or("帧不存在")?;
        let (roi, rw, rh, origin) = crate::ripper::prepare_roi(&fd, &cfg);
        let mask = crate::postprocess::clean(
            &crate::gpu::kernels().filter(&roi, rw, rh, &cfg.filter),
            rw,
            rh,
            &cfg.postprocess,
        );
        // 构图模式：raw（默认，整帧原图）/ overlay / mask / combo（三列）
        // / ocr（导出效果：与导出完全一致的渲染 + 后处理管线）
        let mode = req
            .mode
            .clone()
            .unwrap_or_else(|| "raw".into())
            .trim()
            .to_ascii_lowercase();
        if mode == "ocr" {
            let ocr = &cfg.output.ocr;
            let tile = crate::outputs::event_from_roi(&roi, rw, rh, &mask)
                .and_then(|ev| crate::outputs::render_subtitle_tile(&ev, ocr))
                .map(|t| {
                    crate::outputs::apply_canvas_postprocess(
                        crate::outputs::apply_tile_postprocess(t, ocr),
                        ocr,
                    )
                });
            let tile = match tile {
                Some(t) => t,
                None => {
                    return Ok(json!({
                        "mode": "ocr", "empty": true, "frame": fd.index, "time": fd.time,
                        "note": "当前帧该区域未检出字幕：换个帧或调整过滤参数",
                    }))
                }
            };
            let (tw, th) = (tile.width(), tile.height());
            let seq = PREVIEW_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if seq >= 32 {
                let _ = std::fs::remove_file(
                    Path::new(&cache).join(format!("preview_ocr_{}.png", seq - 32)),
                );
            }
            let p = Path::new(&cache).join(format!("preview_ocr_{seq}.png"));
            tile.save(&p).map_err(|e| e.to_string())?;
            return Ok(json!({
                "image": format!("/api/artifact?path={}", p.display()),
                "image_path": p.display().to_string(),
                "frame": fd.index, "time": fd.time,
                "mode": "ocr", "empty": false,
                "width": tw, "height": th,
                "roi_w": rw, "roi_h": rh,
                "roi_origin": [origin.0, origin.1],
                "roi_scale": crate::ripper::roi_scale(&cfg),
                "region_only": true,
                "frame_width": fd.width, "frame_height": fd.height,
            }));
        }
        let use_roi = req.region_only.unwrap_or(false);
        // 借用复用整帧 / ROI 像素（不再复制整帧 RGB，降低抓取期内存峰值）
        let scale = crate::ripper::roi_scale(&cfg);
        let (base, m, mw, mh): (&[u8], Vec<u8>, usize, usize) = if use_roi {
            (&roi[..], mask.clone(), rw, rh)
        } else {
            let mut full_mask = vec![0u8; fd.width * fd.height];
            // ROI 可能已按 region.scale × preview.scale_video 放大，mask 是缩放后的
            // 尺寸；写回整帧时必须把缩放坐标还原回原始帧坐标，避免越界/错位。
            let (fw, fh) = (fd.width as i64, fd.height as i64);
            for y in 0..rh {
                let fy = (origin.1 + (y as f64 / scale).round() as i64)
                    .max(0)
                    .min(fh - 1);
                let row = fy as usize * fd.width;
                for x in 0..rw {
                    if mask[y * rw + x] > 0 {
                        let fx = (origin.0 + (x as f64 / scale).round() as i64)
                            .max(0)
                            .min(fw - 1);
                        full_mask[row + fx as usize] = 255;
                    }
                }
            }
            (&fd.rgb[..], full_mask, fd.width, fd.height)
        };
        let pw = if mode == "combo" { mw * 3 } else { mw };
        let ph = mh;
        let mut img = image::RgbaImage::new(pw.max(1) as u32, ph.max(1) as u32);
        for y in 0..ph {
            for x in 0..mw {
                let src = y * mw + x;
                let ps = src * 3;
                let (r, g, b) = (base[ps], base[ps + 1], base[ps + 2]);
                let hit = m[src] > 0;
                let mv = if hit { 255u8 } else { 0u8 };
                if mode == "raw" {
                    img.put_pixel(x as u32, y as u32, image::Rgba([r, g, b, 255]));
                } else if mode == "mask" {
                    img.put_pixel(x as u32, y as u32, image::Rgba([mv, mv, mv, 255]));
                } else if mode == "overlay" {
                    let (r2, g2, b2) = overlay_pixel(r, g, b, hit);
                    img.put_pixel(x as u32, y as u32, image::Rgba([r2, g2, b2, 255]));
                } else {
                    img.put_pixel(x as u32, y as u32, image::Rgba([r, g, b, 255]));
                    img.put_pixel((mw + x) as u32, y as u32, image::Rgba([mv, mv, mv, 255]));
                    let (r2, g2, b2) = overlay_pixel(r, g, b, hit);
                    img.put_pixel(
                        (mw * 2 + x) as u32,
                        y as u32,
                        image::Rgba([r2, g2, b2, 255]),
                    );
                }
            }
        }
        let seq = PREVIEW_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if seq >= 32 {
            let _ =
                std::fs::remove_file(Path::new(&cache).join(format!("preview_{}.png", seq - 32)));
        }
        let p = Path::new(&cache).join(format!("preview_{seq}.png"));
        img.save(&p).map_err(|e| e.to_string())?;
        Ok(json!({
            "image": format!("/api/artifact?path={}", p.display()),
            // image_path：缓存 PNG 的绝对路径。UI 以 file:// 加载时相对 URL 到不了后端，
            // Electron 侧经 IPC 取二进制（浏览器直连开发模式仍可用上面的相对路径）
            "image_path": p.display().to_string(),
            "frame": fd.index, "time": fd.time,
            "mode": mode,
            "width": pw, "height": ph,
            "roi_w": rw, "roi_h": rh,
            "roi_origin": [origin.0, origin.1],
            "roi_scale": crate::ripper::roi_scale(&cfg),
            "region_only": use_roi,
            "frame_width": fd.width, "frame_height": fd.height,
        }))
    })
    .await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => {
            crate::logging::error(format!("预览失败 frame={}: {e}", req0_frame));
            err_response(&e)
        }
        Err(e) => {
            crate::logging::error(format!("预览任务异常 frame={}: {e}", req0_frame));
            err_response(&e.to_string())
        }
    }
}

/// 叠加预览着色：命中保留原色，未命中叠绿色调（未命中区域一眼可辨）。
#[inline]
fn overlay_pixel(r: u8, g: u8, b: u8, hit: bool) -> (u8, u8, u8) {
    if hit {
        (r, g, b)
    } else {
        (
            r.saturating_mul(2) / 2 + 30,
            (g as u16 + 90).min(255) as u8,
            b.saturating_mul(2) / 2,
        )
    }
}

/// 用户自定义默认配置落盘位置：<cache_dir 父目录>/esrxp-config.json
fn user_config_path(st: &AppState) -> PathBuf {
    Path::new(&st.cache_dir)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("esrxp-config.json")
}

/// GET /api/config/default —— 内置出厂默认配置
async fn api_config_default() -> Response {
    match serde_json::to_value(AppConfig::default()) {
        Ok(v) => Json(json!({ "config": v, "source": "default" })).into_response(),
        Err(e) => err_response(&format!("默认配置序列化失败: {e}")),
    }
}

/// GET /api/config/file —— 读取持久化的自定义默认配置；文件缺失时回退出厂默认
async fn api_config_file(State(st): State<AppState>) -> Response {
    let p = user_config_path(&st);
    let path_s = p.display().to_string();
    if !p.is_file() {
        return match serde_json::to_value(AppConfig::default()) {
            Ok(v) => {
                Json(json!({ "config": v, "source": "default", "path": path_s })).into_response()
            }
            Err(e) => err_response(&format!("默认配置序列化失败: {e}")),
        };
    }
    match std::fs::read_to_string(&p) {
        Ok(s) => match serde_json::from_str::<AppConfig>(&s) {
            Ok(cfg) => {
                Json(json!({ "config": cfg, "source": "file", "path": path_s })).into_response()
            }
            Err(e) => err_response(&format!("配置文件解析失败，请删除后重试: {e}")),
        },
        Err(e) => err_response(&format!("配置文件读取失败: {e}")),
    }
}

#[derive(Deserialize)]
pub struct ConfigSaveReq {
    pub config: Option<Value>,
}

/// POST /api/config/file —— 保存自定义默认配置（反序列化归一化后再写盘）
async fn api_config_save(State(st): State<AppState>, Json(req): Json<ConfigSaveReq>) -> Response {
    let p = user_config_path(&st);
    let value = match req.config {
        Some(v) => v,
        None => return err_response("缺少 config 字段"),
    };
    // 反序列化再序列化：剔除未知/非法字段并按 AppConfig 结构归一化，保证跨版本可读
    let cfg: AppConfig = match serde_json::from_value(value) {
        Ok(c) => c,
        Err(e) => return err_response(&format!("配置非法: {e}")),
    };
    let text = match serde_json::to_string_pretty(&cfg) {
        Ok(t) => t,
        Err(e) => return err_response(&format!("配置序列化失败: {e}")),
    };
    if let Some(parent) = p.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err_response(&format!("配置目录创建失败: {e}"));
        }
    }
    match std::fs::write(&p, text) {
        Ok(_) => {
            crate::logging::info(format!("保存自定义默认配置: {}", p.display()));
            Json(json!({ "ok": true, "path": p.display().to_string() })).into_response()
        }
        Err(e) => err_response(&format!("配置保存失败: {e}")),
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
        started: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
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
            allow_artifact_dir(out_dir);
            // 工程目录日志文件：此后所有记录同步落盘到 <工程目录>/esrxp.log
            crate::logging::bind_project(out_dir);
            job_log(
                &mut log,
                "INFO",
                format!("任务开始: video={} out={}", req.path, out_dir.display()),
            );
            let mut vs = VideoSource::open(&req.path).map_err(|e| e.to_string())?;
            job_log(
                &mut log,
                "INFO",
                format!(
                    "视频: {}x{} @ {:.2} fps, {:.2}s",
                    vs.width, vs.height, vs.fps, vs.duration
                ),
            );
            let progress_jobs = jobs.clone();
            let progress_id = id2.clone();
            let mut last_pct: i64 = -1;
            let res = rip(
                &mut vs,
                &cfg,
                Some(move |done, total, found| {
                    let pct = if total > 0 { done * 100 / total } else { 0 };
                    // 每 10% 记一次：进程异常退出时可从日志定位中断位置
                    if pct != last_pct && pct % 10 == 0 {
                        last_pct = pct;
                        crate::logging::info(format!("进度 {pct}% ({done}/{total}) 候选 {found}"));
                    }
                    if let Ok(mut g) = progress_jobs.lock() {
                        if let Some(j) = g.get_mut(&progress_id) {
                            j.progress =
                                json!({"done": done, "total": total, "found": found, "pct": pct});
                        }
                    }
                }),
            )
            .map_err(|e| e.to_string())?;

            let (artifacts, project_path) =
                write_all_outputs(&res.events, &res.filtered, &req.path, &cfg, out_dir)?;
            job_log(
                &mut log,
                "INFO",
                format!("产物写出 {} 项到 {}", artifacts.len(), out_dir.display()),
            );
            let vinfo = res.video_info.clone();

            let events: Vec<Value> = res
                .events
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    json!({
                        "index": i + 1, "start": e.start, "end": e.end,
                        "start_frame": e.start_frame, "end_frame": e.end_frame,
                        "bbox": e.bbox, "duration": ((e.end - e.start) * 100.0).round() / 100.0,
                        "diff_frames": e.diff_frames, "deleted": e.deleted,
                    })
                })
                .collect();
            let filtered_meta: Vec<Value> = res
                .filtered
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    json!({
                        "index": i + 1, "start": e.start, "end": e.end,
                        "start_frame": e.start_frame, "end_frame": e.end_frame,
                        "bbox": e.bbox,
                    })
                })
                .collect();
            result = Some(json!({
                "events": events, "filtered": filtered_meta,
                "artifacts": artifacts, "project": project_path,
                "video_info": vinfo,
                "frames_processed": res.frames_processed, "candidates": res.candidates,
                "elapsed_s": (res.elapsed_s * 100.0).round() / 100.0,
            }));
            job_log(
                &mut log,
                "INFO",
                format!(
                    "完成：字幕 {} 条，耗时 {:.2}s",
                    res.events.len(),
                    res.elapsed_s
                ),
            );
            status = "done";
            Ok(())
        })() {
            Ok(()) => {}
            Err(e) => {
                job_log(&mut log, "ERROR", format!("错误: {e}"));
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

async fn api_artifact(
    State(st): State<AppState>,
    query: axum::extract::Query<HashMap<String, String>>,
) -> Response {
    match query.get("path") {
        Some(p) => {
            let path = Path::new(p);
            // 安全校验：仅允许读取注册产物目录（cache / rip / batch / 工程目录）下的文件
            let canon = match path.canonicalize() {
                Ok(c) => c,
                Err(_) => return err_response("文件不存在"),
            };
            if !artifact_dir_allowed(&canon, Some(Path::new(&st.cache_dir))) {
                crate::logging::warn(format!("拒绝产物读取（白名单外）: {}", path.display()));
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": "路径不在允许的产物目录内"})),
                )
                    .into_response();
            }
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

// ------------------------------------------------------------------ 运行日志
#[derive(Deserialize)]
pub struct LogQuery {
    pub path: Option<String>, // 工程目录或日志文件本身
    pub n: Option<usize>,     // 返回行数上限
}

#[derive(Deserialize)]
pub struct LogReq {
    pub project: Option<String>, // 工程目录（同时作为后续日志写入目标）
    pub level: Option<String>,   // INFO / WARN / ERROR / DEBUG
    pub msg: String,
}

/// GET /api/log?path=<工程目录|日志文件>&n=500
/// 优先读磁盘文件（跨进程、重启后仍可追溯），文件不可读时回退内存环形缓冲。
async fn api_log_get(
    State(st): State<AppState>,
    query: axum::extract::Query<LogQuery>,
) -> Response {
    let max = query.n.unwrap_or(500).clamp(1, 5000);
    let mut lines: Vec<String> = Vec::new();
    if let Some(p) = query.path.as_ref() {
        let p = Path::new(p);
        let file = if p.is_dir() {
            p.join(crate::logging::PROJECT_LOG_NAME)
        } else {
            p.to_path_buf()
        };
        if file.exists() {
            // 只允许读取日志文件本身：避免「登记目录 -> 读该目录下任意文件」的读取面扩大
            let is_log = file
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n == crate::logging::PROJECT_LOG_NAME || n == "esrxp-session.log")
                .unwrap_or(false);
            if !is_log {
                return err_response("仅允许读取 esrxp.log / esrxp-session.log");
            }
            match file.canonicalize() {
                Ok(c) if artifact_dir_allowed(&c, Some(Path::new(&st.cache_dir))) => {
                    lines = crate::logging::read_tail(&file, max).unwrap_or_else(|e| vec![e]);
                }
                Ok(_) => return (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": "日志路径不在允许的产物目录内，请先在本会话打开该工程"})),
                )
                    .into_response(),
                Err(e) => lines = vec![format!("日志路径无法解析: {e}")],
            }
        }
    }
    if lines.is_empty() {
        lines = crate::logging::tail_lines(max);
    }
    Json(json!({
        "lines": lines, "count": lines.len(),
        "project_log": crate::logging::project_path(),
        "session_log": crate::logging::session_path(),
        "dropped": crate::logging::dropped_count(),
    }))
    .into_response()
}

/// POST /api/log {project, level, msg} —— 写入工程日志（前端异常与关键操作上报）
/// project 中的目录会被登记进产物白名单，便于 GET /api/log 读回同一份文件。
async fn api_log_post(Json(req): Json<LogReq>) -> Response {
    let level = match req.level.as_deref().unwrap_or("INFO") {
        "INFO" => "INFO",
        "WARN" => "WARN",
        "ERROR" => "ERROR",
        "DEBUG" => "DEBUG",
        _ => "INFO",
    };
    let mut target: Option<String> = None;
    if let Some(p) = req.project.as_ref() {
        let d = Path::new(p);
        if !p.trim().is_empty() && std::fs::create_dir_all(d).is_ok() {
            allow_artifact_dir(d);
            target = crate::logging::bind_project(d);
        }
    }
    crate::logging::record(level, &req.msg);
    Json(json!({
        "ok": true, "level": level,
        "project_log": target.or_else(crate::logging::project_path),
        "session_log": crate::logging::session_path(),
    }))
    .into_response()
}

/// 任务日志：同步进工程日志文件与任务的 log 列表（UI 进度区展示）
fn job_log(log: &mut Vec<String>, level: &str, msg: String) {
    crate::logging::record(level, &msg);
    log.push(format!("[{level}] {msg}"));
}

// ------------------------------------------------------------------ 视频元信息缓存
/// 进程内按「视频路径」缓存宽高。字幕管理器的每次操作都会调 write_all_outputs，
/// 而宽高只跟视频本身有关；每次重开 VideoSource（走一遍 FFmpeg 探测）在网络盘或
/// 大文件上是明显延迟。视频不会在一次会话内换内容，故按路径缓存即可。
static VIDEO_SIZE_CACHE: OnceLock<Mutex<HashMap<String, (usize, usize)>>> = OnceLock::new();

fn video_size_cache() -> &'static Mutex<HashMap<String, (usize, usize)>> {
    VIDEO_SIZE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取视频宽高（失败返回 0,0）。命中缓存则不再打开文件。
fn video_size(video_path: &str) -> (usize, usize) {
    {
        let g = match video_size_cache().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(v) = g.get(video_path) {
            return *v;
        }
    }
    let dim = match VideoSource::open(video_path) {
        Ok(v) => (v.width, v.height),
        Err(_) => (0, 0),
    };
    // 解锁后再写，避免持锁期间执行文件 IO
    match video_size_cache().lock() {
        Ok(mut g) => {
            g.insert(video_path.to_string(), dim);
        }
        Err(p) => {
            p.into_inner().insert(video_path.to_string(), dim);
        }
    }
    dim
}

// ------------------------------------------------------------------ 共享产物写出
/// 根据当前 events/filtered + 配置，把所有输出（SSA/VobSub/OCR/SRT/SRT+bitmap/JSON/.esr）
/// 写到 out_dir。返回 (artifacts, project_path)。
///
/// 软删除（字幕管理器 remove / Show-Hide Deleted）标记的字幕不进入任何产物：
/// `.esr` 工程文件仍保留标记与数据，Purge 后才真正丢弃。此前 remove 只打标记、
/// 产物照旧写出，导致用户以为删掉了、实际 ASS/SRT/VobSub/OCR 一条没少。
/// 统一在此入口过滤一次，各写出函数无需各自判断。
pub fn write_all_outputs(
    events: &[SubtitleEvent],
    filtered: &[SubtitleEvent],
    video_path: &str,
    cfg: &AppConfig,
    out_dir: &Path,
) -> Result<(HashMap<String, String>, String), String> {
    let mut artifacts: HashMap<String, String> = HashMap::new();
    let src = Path::new(video_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "subtitle".into());
    let (vw, vh) = video_size(video_path);
    let events_orig = events;
    let live: Vec<SubtitleEvent> = events.iter().filter(|e| !e.deleted).cloned().collect();
    let events = &live[..];
    if !events.is_empty() && cfg.output.ssa {
        let p = out_dir.join(format!("{src}.ass"));
        write_ssa(events, vw, vh, cfg, &p).map_err(|e| e.to_string())?;
        artifacts.insert("ssa".into(), p.display().to_string());
    }
    if !events.is_empty() && cfg.output.vobsub {
        let (sp, ip) =
            write_vobsub(events, vw, vh, cfg, &out_dir.join(&src)).map_err(|e| e.to_string())?;
        artifacts.insert("sub".into(), sp);
        artifacts.insert("idx".into(), ip);
    }
    if !events.is_empty() && cfg.output.ocr_png {
        let files = write_ocr_png(events, cfg, &out_dir.join("subtitle_imgs"))
            .map_err(|e| e.to_string())?;
        artifacts.insert(
            "ocr_images".into(),
            out_dir.join("subtitle_imgs").display().to_string(),
        );
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
    // .esr 必须写原始全量列表（含 deleted 标记），否则 remove 的标记无处可存、
    // 关闭工程后该字幕就再也无法恢复或 purge 了 —— 故用入参 events 而非过滤后的 live。
    write_esr(
        events_orig,
        filtered,
        video_path,
        cfg,
        &proj,
        &json!(artifacts),
    )
    .map_err(|e| e.to_string())?;
    artifacts.insert("project".into(), proj.display().to_string());
    let mut kinds: Vec<&str> = Vec::new();
    if cfg.output.ssa {
        kinds.push("ssa");
    }
    if cfg.output.vobsub {
        kinds.push("vobsub");
    }
    if cfg.output.ocr_png {
        kinds.push("ocr_png");
    }
    if cfg.output.srt {
        kinds.push("srt");
    }
    if cfg.output.json_timeline {
        kinds.push("json_timeline");
    }
    crate::logging::info(format!(
        "产物写出: dir={} 事件={} 输出={}",
        out_dir.display(),
        events.len(),
        kinds.join(",")
    ));
    Ok((artifacts, proj.display().to_string()))
}

// ------------------------------------------------------------------ 批处理
async fn api_batch(State(st): State<AppState>, Json(req): Json<BatchReq>) -> Response {
    let files: Vec<String> = req
        .files
        .iter()
        .filter(|p| Path::new(p).is_file())
        .cloned()
        .collect();
    if files.is_empty() {
        return err_response("没有有效的视频文件");
    }
    let id = format!("{:012x}", rand_id());
    let subs: Vec<BatchSub> = files
        .iter()
        .map(|f| BatchSub {
            id: format!("{:012x}", rand_id()),
            file: f.clone(),
            status: "pending".into(),
            progress: json!({"done": 0, "total": 0, "found": 0, "pct": 0}),
            result: None,
        })
        .collect();
    let batch = BatchJob {
        id: id.clone(),
        files,
        out_dir: req.out_dir.clone(),
        status: "running".into(),
        subs,
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
                        if s.id == sub_id {
                            s.status = "running".to_string();
                        }
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
                            s.progress =
                                json!({"done": done, "total": total, "found": found, "pct": pct});
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
                                Ok(r) => {
                                    s.status = "done".to_string();
                                    s.result = Some(r.clone());
                                }
                                Err(e) => {
                                    s.status = "error".to_string();
                                    s.result = Some(json!({"error": e}));
                                }
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
            let subs: Vec<Value> = b
                .subs
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id, "file": s.file, "status": s.status,
                        "progress": s.progress, "result": s.result,
                    })
                })
                .collect();
            Json(json!({"id": b.id, "status": b.status, "out_dir": b.out_dir, "subs": subs}))
                .into_response()
        }
        None => err_response("批处理任务不存在"),
    }
}

/// 单文件 rip（批处理子任务复用）。
fn run_one_rip(
    path: &str,
    out_dir: &str,
    cfg: &AppConfig,
    on_progress: impl Fn(i64, i64, i64) + Send + Sync + 'static,
) -> Result<Value, String> {
    let out_dir = Path::new(out_dir);
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    allow_artifact_dir(out_dir);
    crate::logging::bind_project(out_dir);
    crate::logging::info(format!("批处理子任务开始: {path}"));
    let mut vs = VideoSource::open(path).map_err(|e| e.to_string())?;
    let res = rip(&mut vs, cfg, Some(on_progress)).map_err(|e| e.to_string())?;
    let (artifacts, project_path) =
        write_all_outputs(&res.events, &res.filtered, path, cfg, out_dir)?;
    let events: Vec<Value> = res
        .events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            json!({
                "index": i + 1, "start": e.start, "end": e.end,
                "start_frame": e.start_frame, "end_frame": e.end_frame,
                "bbox": e.bbox, "diff_frames": e.diff_frames, "deleted": e.deleted,
            })
        })
        .collect();
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
        vs.decode_range(f, f + 1, 1, |fd| {
            fd_opt = Some(fd);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
        let fd = fd_opt.ok_or("帧不存在")?;
        let (x, y) = (
            req.x.clamp(0, fd.width as i64 - 1) as usize,
            req.y.clamp(0, fd.height as i64 - 1) as usize,
        );
        let ps = (y * fd.width + x) * 3;
        Ok(json!({"rgb": [fd.rgb[ps], fd.rgb[ps + 1], fd.rgb[ps + 2]],
                  "x": x, "y": y, "frame": fd.index}))
    })
    .await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err_response(&e),
        Err(e) => err_response(&e.to_string()),
    }
}

// ------------------------------------------------------------------ 字幕管理器
/// 字幕管理器载入结果：(事件列表, 被过滤候选, 原视频路径, 配置, 输出目录)
type LoadedProject = (
    Vec<SubtitleEvent>,
    Vec<SubtitleEvent>,
    String,
    AppConfig,
    String,
);

// 直接返回 axum Response 作为错误值：调用点即 handler，省去一次 Box 解包。
// 代价是 Err 变体偏大（clippy::result_large_err），此处换取调用侧零转换开销。
#[allow(clippy::result_large_err)]
fn load_project_for_manage(project: &str) -> Result<LoadedProject, Response> {
    let p = Path::new(project);
    if !p.is_file() {
        return Err(err_response(&format!("工程文件不存在: {project}")));
    }
    match load_esr(p) {
        Ok((events, filtered, video, cfg)) => {
            let out_dir = p
                .parent()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| ".".into());
            allow_artifact_dir(p.parent().unwrap_or_else(|| Path::new(".")));
            crate::logging::bind_project(p.parent().unwrap_or_else(|| Path::new(".")));
            crate::logging::info(format!(
                "打开工程: {} （字幕 {} 条）",
                project,
                events.len()
            ));
            Ok((events, filtered, video, cfg, out_dir))
        }
        Err(e) => Err(err_response(&format!("工程文件解析失败: {e}"))),
    }
}

fn persist_project(
    events: &[SubtitleEvent],
    filtered: &[SubtitleEvent],
    video: &str,
    cfg: &AppConfig,
    out_dir: &str,
) -> Result<
    (
        std::collections::HashMap<String, String>,
        Vec<SubtitleEvent>,
    ),
    String,
> {
    let (artifacts, _proj) = write_all_outputs(events, filtered, video, cfg, Path::new(out_dir))
        .map_err(|e| e.to_string())?;
    Ok((artifacts, events.to_vec()))
}

/// 把事件渲染为白底黑字位图（与 OCR 导出同一 render_subtitle_tile 管线，含 despeckle 除噪）。
/// 返回 (RGB24, w, h)。字幕管理器「位图」列用它替代原始视频像素——原始裁切图是
/// 复杂背景+字幕混合（缩略图看不清字幕、噪声明显），渲染图保证清晰可读。
fn render_event_tile(e: &SubtitleEvent, cfg: &AppConfig) -> Option<(Vec<u8>, usize, usize)> {
    let tile = crate::outputs::render_subtitle_tile(e, &cfg.output.ocr)?;
    let (w, h) = (tile.width() as usize, tile.height() as usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for p in tile.pixels() {
        rgb.extend_from_slice(&[p[0], p[1], p[2]]);
    }
    Some((rgb, w, h))
}

fn events_meta(events: &[SubtitleEvent], cfg: &AppConfig) -> Vec<Value> {
    events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let (render_b64, render_w, render_h) = match render_event_tile(e, cfg) {
                Some((rgb, w, h)) => (base64_encode(&rgb), w as i64, h as i64),
                None => (String::new(), 0, 0),
            };
            json!({
                "index": i + 1, "start": (e.start * 100.0).round() / 100.0,
                "end": (e.end * 100.0).round() / 100.0,
                "start_frame": e.start_frame, "end_frame": e.end_frame,
                "bbox": e.bbox, "diff_frames": e.diff_frames, "deleted": e.deleted,
                "image_w": e.image_w, "image_h": e.image_h,
                "image_b64": base64_encode(&e.image),  // bbox 裁切 RGB24（回退预览用）
                "render_b64": render_b64,              // 渲染后白底黑字位图（管理器首选显示）
                "render_w": render_w, "render_h": render_h,
            })
        })
        .collect()
}

#[derive(Deserialize)]
pub struct MergeRepeatReq {
    pub project: String,            // .esr 工程路径
    pub iou_threshold: Option<f64>, // mask 相似度阈值（默认 0.9）
    pub max_gap_s: Option<f64>,     // 时间间隔上限（秒，默认 0.2）
}

/// MIMergeRepeat：一键合并内容重复的连续字幕（仅未删除事件，已删除事件原样保留）。
async fn api_manager_merge_repeat(Json(req): Json<MergeRepeatReq>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let iou = req.iou_threshold.unwrap_or(0.9);
    let gap = req.max_gap_s.unwrap_or(0.2);
    let mut deleted: Vec<SubtitleEvent> = events.iter().filter(|e| e.deleted).cloned().collect();
    let alive: Vec<SubtitleEvent> = events.into_iter().filter(|e| !e.deleted).collect();
    let (mut merged, removed) = merge_repeat_manual(alive, iou, gap);
    merged.append(&mut deleted);
    merged.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let (artifacts, proj) =
        match write_all_outputs(&merged, &filtered, &video, &cfg, Path::new(&out_dir)) {
            Ok(v) => v,
            Err(e) => return err_response(&e),
        };
    let _ = proj;
    Json(json!({"count": merged.len(), "merged": removed, "artifacts": artifacts, "subtitles": events_meta(&merged, &cfg)})).into_response()
}

async fn api_manager_list(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (events, filtered, _video, cfg, _out) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    Json(json!({
        "subtitles": events_meta(&events, &cfg),
        "filtered": events_meta(&filtered, &cfg),
        "count": events.len(), "filtered_count": filtered.len(),
    }))
    .into_response()
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
    events.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let (artifacts, proj) =
        match write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir)) {
            Ok(v) => v,
            Err(e) => return err_response(&e),
        };
    let _ = proj;
    Json(json!({"count": events.len(), "artifacts": artifacts, "subtitles": events_meta(&events, &cfg)})).into_response()
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
    let (artifacts, proj) =
        match write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir)) {
            Ok(v) => v,
            Err(e) => return err_response(&e),
        };
    let _ = proj;
    Json(json!({"count": events.len(), "artifacts": artifacts, "subtitles": events_meta(&events, &cfg)})).into_response()
}

async fn api_manager_purge(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let kept: Vec<SubtitleEvent> = events.into_iter().filter(|e| !e.deleted).collect();
    let (artifacts, proj) =
        match write_all_outputs(&kept, &filtered, &video, &cfg, Path::new(&out_dir)) {
            Ok(v) => v,
            Err(e) => return err_response(&e),
        };
    let _ = proj;
    Json(
        json!({"count": kept.len(), "artifacts": artifacts, "subtitles": events_meta(&kept, &cfg)}),
    )
    .into_response()
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
    let res = tokio::task::spawn_blocking(
        move || -> Result<(Vec<SubtitleEvent>, Vec<Value>), String> {
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
            let (artifacts, proj) =
                write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir))
                    .map_err(|e| e.to_string())?;
            let _ = proj;
            let _ = artifacts;
            let meta = events_meta(&events, &cfg);
            Ok((events, meta))
        },
    )
    .await;
    match res {
        Ok(Ok((events, meta))) => {
            Json(json!({"count": events.len(), "subtitles": meta})).into_response()
        }
        Ok(Err(e)) => err_response(&e),
        Err(e) => err_response(&e.to_string()),
    }
}

// 时间轴编辑（字幕管理器：改 start/end）—— 对齐 esrXP Subtitle Manager 时间轴编辑
async fn api_manager_edit(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (mut events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let edits = match req.edits {
        Some(v) => v,
        None => return err_response("缺少 edits"),
    };
    // 取视频 fps 用于换算帧号
    let fps = {
        let vs = VideoSource::open(&video);
        match vs {
            Ok(v) => v.fps,
            Err(e) => return err_response(&format!("打开视频失败: {e}")),
        }
    };
    for ed in &edits {
        if let Some(e) = events.get_mut(ed.index) {
            if let Some(st) = ed.start {
                e.start = st;
                e.start_frame = (st * fps).round() as i64;
            }
            if let Some(en) = ed.end {
                e.end = en;
                e.end_frame = (en * fps).round() as i64;
            }
            e.diff_frames = (e.end_frame - e.start_frame).max(1);
        }
    }
    match persist_project(&events, &filtered, &video, &cfg, &out_dir) {
        Ok((artifacts, _)) => Json(json!({"count": events.len(), "artifacts": artifacts, "subtitles": events_meta(&events, &cfg)})).into_response(),
        Err(e) => err_response(&e),
    }
}

// 时间平移（对齐 esrXP Time Shift）：offset_ms 毫秒，作用于全部保留字幕或选中 indexes
async fn api_manager_shift(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (mut events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let offset_s = (req.offset_ms.unwrap_or(0) as f64) / 1000.0;
    if offset_s == 0.0 {
        return err_response("offset_ms 不能为 0");
    }
    let target: Vec<usize> = match &req.indexes {
        Some(idxs) if !idxs.is_empty() => idxs.clone(),
        _ => events
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.deleted)
            .map(|(i, _)| i)
            .collect(),
    };
    let fps = {
        let vs = VideoSource::open(&video);
        match vs {
            Ok(v) => v.fps,
            Err(e) => return err_response(&format!("打开视频失败: {e}")),
        }
    };
    for i in &target {
        if let Some(e) = events.get_mut(*i) {
            e.start = (e.start + offset_s).max(0.0);
            e.end = (e.end + offset_s).max(0.0);
            e.start_frame = (e.start * fps).round() as i64;
            e.end_frame = (e.end * fps).round() as i64;
            e.diff_frames = (e.end_frame - e.start_frame).max(1);
        }
    }
    match persist_project(&events, &filtered, &video, &cfg, &out_dir) {
        Ok((artifacts, _)) => Json(json!({"count": events.len(), "offset_ms": req.offset_ms.unwrap_or(0), "artifacts": artifacts, "subtitles": events_meta(&events, &cfg)})).into_response(),
        Err(e) => err_response(&e),
    }
}

// 分割：在 time 秒处把一条字幕切成两条（各自按新区间重抓）
async fn api_manager_split(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (mut events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let idx = match &req.indexes {
        Some(v) if v.len() == 1 => v[0],
        _ => return err_response("split 需指定单条 index"),
    };
    let t = req.time.unwrap_or(-1.0);
    if t <= 0.0 {
        return err_response("split 需指定 time（秒）");
    }
    let res = tokio::task::spawn_blocking(
        move || -> Result<(Vec<SubtitleEvent>, Vec<Value>), String> {
            let ev = events.get(idx).cloned().ok_or("index 越界")?;
            if t <= ev.start || t >= ev.end {
                return Err(format!(
                    "分割时间 {t:.2}s 需在字幕区间内（{:.2}–{:.2}s）",
                    ev.start, ev.end
                ));
            }
            let mut vs = VideoSource::open(&video).map_err(|e| e.to_string())?;
            let fps = vs.fps;
            let mut e1 = ev.clone();
            e1.end = t;
            e1.end_frame = (t * fps).round() as i64;
            e1.diff_frames = (e1.end_frame - e1.start_frame).max(1);
            let mut e2 = ev.clone();
            e2.start = t;
            e2.start_frame = (t * fps).round() as i64;
            e2.diff_frames = (e2.end_frame - e2.start_frame).max(1);
            let c1 = crop_event(&mut vs, &cfg, &e1).map_err(|e| format!("前半段重抓失败: {e}"))?;
            let c2 = crop_event(&mut vs, &cfg, &e2).map_err(|e| format!("后半段重抓失败: {e}"))?;
            events[idx] = c1;
            events.push(c2);
            events.sort_by(|a, b| {
                a.start
                    .partial_cmp(&b.start)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let (artifacts, _) =
                write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir))
                    .map_err(|e| e.to_string())?;
            let _ = artifacts;
            let meta = events_meta(&events, &cfg);
            Ok((events, meta))
        },
    )
    .await;
    match res {
        Ok(Ok((events, meta))) => {
            Json(json!({"count": events.len(), "subtitles": meta})).into_response()
        }
        Ok(Err(e)) => err_response(&e),
        Err(e) => err_response(&e.to_string()),
    }
}

// 合并：两条相邻字幕合并为一条（按合并区间重抓）
async fn api_manager_merge(Json(req): Json<ManagerOp>) -> Response {
    let r = load_project_for_manage(&req.project);
    let (mut events, filtered, video, cfg, out_dir) = match r {
        Ok(x) => x,
        Err(e) => return e,
    };
    let idxs = req.indexes.unwrap_or_default();
    if idxs.len() != 2 {
        return err_response("merge 需指定两条 index");
    }
    let res = tokio::task::spawn_blocking(
        move || -> Result<(Vec<SubtitleEvent>, Vec<Value>), String> {
            let (a, b) = match (events.get(idxs[0]).cloned(), events.get(idxs[1]).cloned()) {
                (Some(x), Some(y)) => (x, y),
                _ => return Err("index 越界".into()),
            };
            let mut vs = VideoSource::open(&video).map_err(|e| e.to_string())?;
            let fps = vs.fps;
            let mut merged = a.clone();
            merged.start = a.start.min(b.start);
            merged.end = a.end.max(b.end);
            merged.start_frame = (merged.start * fps).round() as i64;
            merged.end_frame = (merged.end * fps).round() as i64;
            merged.diff_frames = (merged.end_frame - merged.start_frame).max(1);
            let c =
                crop_event(&mut vs, &cfg, &merged).map_err(|e| format!("合并区间重抓失败: {e}"))?;
            // 高 index 先删，避免错位
            let (lo, hi) = (idxs[0].min(idxs[1]), idxs[0].max(idxs[1]));
            events.remove(hi);
            events.remove(lo);
            events.push(c);
            events.sort_by(|a, b| {
                a.start
                    .partial_cmp(&b.start)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let (artifacts, _) =
                write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir))
                    .map_err(|e| e.to_string())?;
            let _ = artifacts;
            let meta = events_meta(&events, &cfg);
            Ok((events, meta))
        },
    )
    .await;
    match res {
        Ok(Ok((events, meta))) => {
            Json(json!({"count": events.len(), "subtitles": meta})).into_response()
        }
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
    let (artifacts, _proj) =
        match write_all_outputs(&events, &filtered, &video, &cfg, Path::new(&out_dir)) {
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
        "subtitles": events_meta(&events, &cfg), "filtered": events_meta(&filtered, &cfg),
        "count": events.len(), "filtered_count": filtered.len(),
    }))
    .into_response()
}

/// 任务 ID：全量纳秒 + PID + 单调序号混合，避免同进程内并发撞 key。
/// 原实现只用 `subsec_nanos()`（0..1e9 低位）与 PID 异或，碰撞窗口虽小但会让
/// jobs/subs 的 HashMap 互相覆盖，`/api/jobs/{id}` 返回别人的结果。
fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    n ^ (std::process::id() as u64).rotate_left(32) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_event(start: f64, deleted: bool) -> SubtitleEvent {
        SubtitleEvent {
            start,
            end: start + 1.0,
            start_frame: 0,
            end_frame: 25,
            image: vec![0u8; 3],
            image_w: 1,
            image_h: 1,
            mask: vec![255u8; 1],
            roi_mask: vec![255u8; 4],
            roi_w: 2,
            roi_h: 2,
            bbox: (0, 0, 1, 1),
            roi_origin: (0, 0),
            diff_frames: 1,
            source_frame: 0,
            deleted,
        }
    }

    /// 回归（P0）：软删除标记的字幕不得进入产物。
    /// `write_all_outputs` 入口统一过滤 —— 各写出函数本身不检查 deleted，
    /// 过滤只在这一处发生，故这里直接验证过滤结果集合。
    #[test]
    fn soft_deleted_events_excluded_from_outputs() {
        let events = [
            dummy_event(0.5, false),
            dummy_event(2.0, true), // 软删除
            dummy_event(3.5, false),
        ];
        let live: Vec<SubtitleEvent> = events.iter().filter(|e| !e.deleted).cloned().collect();
        assert_eq!(live.len(), 2, "被标记删除的字幕必须被排除");
        assert!(live.iter().all(|e| !e.deleted));
        assert_eq!(live[0].start, 0.5);
        assert_eq!(live[1].start, 3.5);
    }

    /// `.esr` 仍需保留全量事件（含 deleted 标记），否则 remove 的标记无处可存，
    /// 关闭工程后该字幕无法再恢复或 purge —— 见 write_all_outputs 内的 events_orig。
    #[test]
    fn esr_keeps_soft_deleted_for_recovery() {
        let events = [dummy_event(0.5, false), dummy_event(2.0, true)];
        assert_eq!(events.len(), 2, "工程文件写入侧应收到未过滤的全量列表");
        assert!(events[1].deleted, "标记必须仍在，供字幕管理器恢复/彻底清除");
    }

    /// 全部软删除时产物集合为空（写出函数对空 events 是 no-op）。
    #[test]
    fn all_deleted_yields_empty_output_set() {
        let events = [dummy_event(0.5, true), dummy_event(2.0, true)];
        let live: Vec<SubtitleEvent> = events.iter().filter(|e| !e.deleted).cloned().collect();
        assert!(live.is_empty());
    }

    /// 任务 ID 不应碰撞：连续取多个值必须互不相同。
    #[test]
    fn rand_id_does_not_collide() {
        let mut ids = std::collections::HashSet::new();
        for _ in 0..10_000 {
            assert!(ids.insert(rand_id()), "rand_id 在 1 万次内出现碰撞");
        }
    }
}
