//! egui 主应用：状态管理 + 面板布局 + 后台线程通信。
//!
//! MVP 流程：打开视频/工程 → 参数配置 → 预览 → 抓取（进度轮询）→ 产物查看/保存。

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use eframe::egui::{self, ColorImage, TextureOptions};
use serde_json::Value;

use crate::backend::Backend;
use crate::client;
use crate::config::{UiConfig, PREVIEW_MODES};

// ------------------------------------------------------------------ 消息
enum Msg {
    BackendReady(Result<Backend, String>),
    ConfigLoaded(Result<UiConfig, String>),
    VideoOpened(Result<VideoInfo, String>),
    ProjectOpened(Result<ProjectInfo, String>),
    Preview(Result<PreviewData, String>),
    Rip(RipUpdate),
}

// ------------------------------------------------------------------ 数据结构
#[derive(Debug, Clone)]
struct VideoInfo {
    path: String,
    width: usize,
    height: usize,
    fps: f64,
    duration: f64,
    frame_count: i64,
}

#[derive(Debug, Clone)]
struct ProjectInfo {
    video: VideoInfo,
    out_dir: String,
    config: UiConfig,
}

struct PreviewData {
    rgba: Vec<u8>,
    w: usize,
    h: usize,
    frame: i64,
    time: f64,
    empty: bool,
    note: String,
}

#[derive(Debug, Clone)]
struct PreviewMeta {
    w: usize,
    h: usize,
    frame: i64,
    time: f64,
    empty: bool,
    note: String,
}

#[derive(Debug, Clone)]
struct RipUpdate {
    status: String,
    done: i64,
    total: i64,
    found: i64,
    pct: i64,
    result: Option<Value>,
    error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct RipState {
    running: bool,
    done: i64,
    total: i64,
    found: i64,
    pct: i64,
    result: Option<Value>,
    error: Option<String>,
}

#[derive(Clone)]
struct PreviewState {
    frame: i64,
    mode: String,
    region_only: bool,
    busy: bool,
    texture: Option<egui::TextureHandle>,
    meta: Option<PreviewMeta>,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            frame: 0,
            mode: "combo".into(),
            region_only: true,
            busy: false,
            texture: None,
            meta: None,
        }
    }
}

// ------------------------------------------------------------------ 应用
pub struct EsrxpApp {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    backend: Option<Backend>,
    base_url: Option<String>,
    backend_error: Option<String>,
    config: UiConfig,
    video: Option<VideoInfo>,
    out_dir: String,
    preview: PreviewState,
    rip: RipState,
    status: String,
}

impl EsrxpApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = mpsc::channel::<Msg>();
        // 启动后端（子进程）+ 拉取默认配置
        let tx2 = tx.clone();
        let ctx = _cc.egui_ctx.clone();
        std::thread::spawn(move || {
            match Backend::start() {
                Ok(b) => {
                    let url = b.base_url.clone();
                    let _ = tx2.send(Msg::BackendReady(Ok(b)));
                    let cfg = client::get_json(&url, "/api/config/default").and_then(|v| {
                        let cfg_v = v.get("config").cloned().unwrap_or(Value::Null);
                        serde_json::from_value::<UiConfig>(cfg_v).map_err(|e| e.to_string())
                    });
                    let _ = tx2.send(Msg::ConfigLoaded(cfg));
                }
                Err(e) => {
                    let _ = tx2.send(Msg::BackendReady(Err(e)));
                }
            }
            ctx.request_repaint();
        });
        Self {
            tx,
            rx,
            backend: None,
            base_url: None,
            backend_error: None,
            config: UiConfig::default(),
            video: None,
            out_dir: String::new(),
            preview: PreviewState::default(),
            rip: RipState::default(),
            status: "正在启动后端…".into(),
        }
    }

    fn drain_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::BackendReady(Ok(b)) => {
                    self.base_url = Some(b.base_url.clone());
                    self.backend = Some(b);
                    self.backend_error = None;
                    self.status = "后端就绪".into();
                }
                Msg::BackendReady(Err(e)) => {
                    self.backend_error = Some(e.clone());
                    self.status = format!("后端启动失败：{e}");
                }
                Msg::ConfigLoaded(Ok(c)) => {
                    self.config = c;
                    self.status = "配置已载入".into();
                }
                Msg::ConfigLoaded(Err(e)) => {
                    self.status = format!("配置载入失败：{e}");
                }
                Msg::VideoOpened(Ok(vi)) => {
                    self.preview.frame = 0;
                    self.video = Some(vi.clone());
                    if self.out_dir.trim().is_empty() {
                        self.out_dir = default_out_dir(&vi.path);
                    }
                    self.status = format!("已打开视频：{}", vi.path);
                    self.request_preview(ctx);
                }
                Msg::VideoOpened(Err(e)) => {
                    self.status = format!("打开视频失败：{e}");
                }
                Msg::ProjectOpened(Ok(p)) => {
                    self.preview.frame = 0;
                    self.video = Some(p.video.clone());
                    self.out_dir = p.out_dir.clone();
                    self.config = p.config.clone();
                    self.status = format!("已打开工程：{}", p.video.path);
                    self.request_preview(ctx);
                }
                Msg::ProjectOpened(Err(e)) => {
                    self.status = format!("打开工程失败：{e}");
                }
                Msg::Preview(Ok(pd)) => {
                    self.preview.busy = false;
                    if pd.empty {
                        self.preview.texture = None;
                        self.preview.meta = Some(PreviewMeta {
                            w: 0,
                            h: 0,
                            frame: pd.frame,
                            time: pd.time,
                            empty: true,
                            note: pd.note.clone(),
                        });
                        self.status = format!("预览（空）：{}", pd.note);
                    } else {
                        let img = ColorImage::from_rgba_unmultiplied([pd.w, pd.h], &pd.rgba);
                        let tex = ctx.load_texture("preview", img, TextureOptions::LINEAR);
                        self.preview.texture = Some(tex);
                        self.preview.meta = Some(PreviewMeta {
                            w: pd.w,
                            h: pd.h,
                            frame: pd.frame,
                            time: pd.time,
                            empty: false,
                            note: String::new(),
                        });
                        self.status =
                            format!("预览帧 {} @ {:.2}s（{}×{}）", pd.frame, pd.time, pd.w, pd.h);
                    }
                }
                Msg::Preview(Err(e)) => {
                    self.preview.busy = false;
                    self.status = format!("预览失败：{e}");
                }
                Msg::Rip(u) => {
                    self.rip.done = u.done;
                    self.rip.total = u.total;
                    self.rip.found = u.found;
                    self.rip.pct = u.pct;
                    self.rip.running = u.status == "running";
                    if u.status != "running" {
                        self.rip.result = u.result;
                        self.rip.error = u.error.clone();
                        self.status = match u.status.as_str() {
                            "done" => "抓取完成".into(),
                            _ => {
                                format!("抓取失败：{}", u.error.unwrap_or_else(|| u.status.clone()))
                            }
                        };
                    }
                }
            }
        }
    }

    fn request_preview(&mut self, ctx: &egui::Context) {
        if self.preview.busy {
            return;
        }
        let Some(base) = self.base_url.clone() else {
            self.status = "后端未就绪，无法预览".into();
            return;
        };
        let Some(video) = self.video.clone() else {
            self.status = "请先打开视频或工程".into();
            return;
        };
        let frame = self.preview.frame;
        let mode = self.preview.mode.clone();
        let region_only = self.preview.region_only;
        let config = self.config.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        self.preview.busy = true;
        std::thread::spawn(move || {
            let res = fetch_preview(&base, &video, frame, &mode, region_only, &config);
            let _ = tx.send(Msg::Preview(res));
            ctx2.request_repaint();
        });
    }

    fn start_rip(&mut self, ctx: &egui::Context) {
        if self.rip.running {
            return;
        }
        let Some(base) = self.base_url.clone() else {
            self.status = "后端未就绪".into();
            return;
        };
        let Some(video) = self.video.clone() else {
            self.status = "请先打开视频".into();
            return;
        };
        let out_dir = self.out_dir.trim().to_string();
        if out_dir.is_empty() {
            self.status = "请先填写输出目录".into();
            return;
        }
        let config = self.config.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        self.rip = RipState {
            running: true,
            ..Default::default()
        };
        std::thread::spawn(move || {
            let body = serde_json::json!({
                "path": video.path,
                "out_dir": out_dir,
                "config": serde_json::to_value(&config).unwrap_or(Value::Null),
            });
            let job_id = match client::post_json(&base, "/api/rip", &body).and_then(|v| {
                v.get("job_id")
                    .and_then(|j| j.as_str())
                    .map(String::from)
                    .ok_or_else(|| "响应缺少 job_id".to_string())
            }) {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.send(Msg::Rip(RipUpdate {
                        status: "error".into(),
                        done: 0,
                        total: 0,
                        found: 0,
                        pct: 0,
                        result: None,
                        error: Some(e),
                    }));
                    ctx2.request_repaint();
                    return;
                }
            };
            // 轮询任务进度
            loop {
                let upd = client::get_json(&base, &format!("/api/jobs/{job_id}"))
                    .map(|v| {
                        let status = v
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let prog = v.get("progress").cloned().unwrap_or(Value::Null);
                        let g = |k: &str| prog.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
                        RipUpdate {
                            status: status.clone(),
                            done: g("done"),
                            total: g("total"),
                            found: g("found"),
                            pct: g("pct"),
                            result: v.get("result").cloned(),
                            error: v
                                .get("log")
                                .and_then(|l| l.as_array())
                                .and_then(|a| a.last())
                                .and_then(|x| x.as_str())
                                .map(String::from),
                        }
                    })
                    .unwrap_or_else(|e| RipUpdate {
                        status: "error".into(),
                        done: 0,
                        total: 0,
                        found: 0,
                        pct: 0,
                        result: None,
                        error: Some(e),
                    });
                let done = upd.status != "running";
                let _ = tx.send(Msg::Rip(upd));
                ctx2.request_repaint();
                if done {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
    }
}

// ------------------------------------------------------------------ 工作线程辅助

/// 拉取预览：POST /api/preview → 下载缓存 PNG → 解码为 RGBA。
fn fetch_preview(
    base: &str,
    video: &VideoInfo,
    frame: i64,
    mode: &str,
    region_only: bool,
    config: &UiConfig,
) -> Result<PreviewData, String> {
    let body = serde_json::json!({
        "path": video.path,
        "frame": frame,
        "config": serde_json::to_value(config).map_err(|e| e.to_string())?,
        "region_only": region_only,
        "mode": mode,
    });
    let v = client::post_json(base, "/api/preview", &body)?;
    let fno = v.get("frame").and_then(|x| x.as_i64()).unwrap_or(frame);
    let t = v.get("time").and_then(|x| x.as_f64()).unwrap_or(0.0);
    if v.get("empty").and_then(|e| e.as_bool()).unwrap_or(false) {
        return Ok(PreviewData {
            rgba: vec![],
            w: 0,
            h: 0,
            frame: fno,
            time: t,
            empty: true,
            note: v
                .get("note")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    let img_url = v
        .get("image")
        .and_then(|i| i.as_str())
        .ok_or("预览响应缺少 image")?;
    let bytes = client::get_bytes(base, img_url)?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    Ok(PreviewData {
        rgba: img.into_raw(),
        w,
        h,
        frame: fno,
        time: t,
        empty: false,
        note: String::new(),
    })
}

fn parse_video_info(v: &Value) -> Option<VideoInfo> {
    Some(VideoInfo {
        path: v.get("path")?.as_str()?.to_string(),
        width: v.get("width")?.as_i64()? as usize,
        height: v.get("height")?.as_i64()? as usize,
        fps: v.get("fps").and_then(|x| x.as_f64()).unwrap_or(0.0),
        duration: v.get("duration_s").and_then(|x| x.as_f64()).unwrap_or(0.0),
        frame_count: v.get("frame_count").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

fn default_out_dir(video_path: &str) -> String {
    let p = Path::new(video_path);
    let parent = p.parent().unwrap_or(Path::new("."));
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    parent.join(format!("{stem}.esrxp")).display().to_string()
}

/// 在系统文件管理器中打开路径（跨平台：xdg-open / explorer / open）。
/// 文件用默认关联应用打开，目录用资源管理器打开。
fn open_path(path: &str) {
    let cmd = match std::env::consts::OS {
        "windows" => "explorer",
        "macos" => "open",
        _ => "xdg-open",
    };
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

// ------------------------------------------------------------------ eframe App

impl eframe::App for EsrxpApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_messages(ctx);

        // —— 顶部：打开 + 视频信息 ——
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("打开视频…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(
                            "视频",
                            &[
                                "mp4", "mkv", "avi", "mov", "wmv", "flv", "ts", "m2ts", "webm",
                                "rmvb", "mpg", "mpeg",
                            ],
                        )
                        .pick_file()
                    {
                        self.open_video(path.to_string_lossy().to_string(), ctx);
                    }
                }
                if ui.button("打开工程 (.esr)…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("esrxp 工程", &["esr"])
                        .pick_file()
                    {
                        self.open_project(path.to_string_lossy().to_string(), ctx);
                    }
                }
                ui.separator();
                if let Some(v) = &self.video {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  {}×{}  {:.2}fps  {:.1}s  {}帧",
                            v.path, v.width, v.height, v.fps, v.duration, v.frame_count
                        ))
                        .small(),
                    );
                } else {
                    ui.label(egui::RichText::new("未打开视频").weak());
                }
                ui.separator();
                ui.label(egui::RichText::new(&self.status).weak());
                if self.backend_error.is_some() {
                    let mut b = self.backend.take();
                    if ui.button("重启后端").clicked() {
                        if let Some(mut bm) = b.take() {
                            match bm.restart() {
                                Ok(()) => {
                                    let url = bm.base_url.clone();
                                    self.backend = Some(bm);
                                    self.base_url = Some(url);
                                    self.backend_error = None;
                                    self.status = "后端已重启".into();
                                }
                                Err(e) => self.status = format!("重启后端失败：{e}"),
                            }
                        } else {
                            let tx = self.tx.clone();
                            let ctx2 = ctx.clone();
                            std::thread::spawn(move || {
                                let res = Backend::start();
                                let _ = tx.send(Msg::BackendReady(res));
                                ctx2.request_repaint();
                            });
                        }
                    } else {
                        self.backend = b;
                    }
                }
            });
        });

        // —— 左侧：参数 ——
        egui::SidePanel::left("params")
            .resizable(true)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.heading("参数");
                ui.horizontal(|ui| {
                    if ui.button("保存为默认").clicked() {
                        self.save_default_config(ctx);
                    }
                    if ui.button("恢复出厂").clicked() {
                        self.load_default_config(ctx);
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("params_scroll")
                    .show(ui, |ui| {
                        crate::config_ui::all_sections(ui, &mut self.config);
                    });
            });

        // —— 中央：预览 ——
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("帧");
                let max_frame = self
                    .video
                    .as_ref()
                    .map(|v| (v.frame_count - 1).max(0))
                    .unwrap_or(0);
                ui.add_enabled(
                    self.video.is_some(),
                    egui::Slider::new(&mut self.preview.frame, 0..=max_frame),
                );
                ui.label("模式");
                egui::ComboBox::from_id_salt("preview_mode")
                    .selected_text(self.preview.mode.as_str())
                    .show_ui(ui, |ui| {
                        for m in PREVIEW_MODES {
                            ui.selectable_value(&mut self.preview.mode, m.to_owned(), m);
                        }
                    });
                ui.checkbox(&mut self.preview.region_only, "仅区域");
                let refresh = ui.add_enabled(
                    self.video.is_some() && !self.preview.busy,
                    egui::Button::new(if self.preview.busy {
                        "预览中…"
                    } else {
                        "刷新预览"
                    }),
                );
                if refresh.clicked() {
                    self.request_preview(ctx);
                }
                if let Some(m) = &self.preview.meta {
                    ui.label(
                        egui::RichText::new(format!(
                            "帧 {} @ {:.2}s · {}×{}",
                            m.frame, m.time, m.w, m.h
                        ))
                        .weak()
                        .small(),
                    );
                }
            });
            ui.separator();
            egui::ScrollArea::both()
                .id_salt("preview_scroll")
                .show(ui, |ui| {
                    if let Some(tex) = &self.preview.texture {
                        let avail = ui.available_size();
                        let (tw, th) = (tex.size()[0] as f32, tex.size()[1] as f32);
                        let scale = if tw > 0.0 && th > 0.0 {
                            (avail.x / tw).min(avail.y / th).min(1.0).max(0.01)
                        } else {
                            1.0
                        };
                        let size = egui::vec2((tw * scale).max(1.0), (th * scale).max(1.0));
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Image::new(tex).fit_to_exact_size(size));
                        });
                    } else if let Some(m) = &self.preview.meta {
                        if m.empty {
                            ui.label(
                                egui::RichText::new(format!("当前帧未检出字幕：{}", m.note)).weak(),
                            );
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(
                                "打开视频后在此预览（模式 raw / mask / overlay / combo / ocr）",
                            )
                            .weak(),
                        );
                    }
                });
        });

        // —— 底部：抓取 + 产物 ——
        egui::TopBottomPanel::bottom("bottom")
            .resizable(true)
            .default_height(240.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("输出目录");
                    ui.add_enabled(
                        !self.rip.running,
                        egui::TextEdit::singleline(&mut self.out_dir)
                            .desired_width((ui.available_width() - 280.0).max(100.0)),
                    );
                    if ui
                        .add_enabled(!self.rip.running, egui::Button::new("浏览…"))
                        .clicked()
                    {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.out_dir = dir.display().to_string();
                        }
                    }
                    let rip_btn = ui.add_enabled(
                        self.video.is_some() && !self.rip.running && self.base_url.is_some(),
                        egui::Button::new(if self.rip.running {
                            "抓取中…"
                        } else {
                            "开始抓取"
                        }),
                    );
                    if rip_btn.clicked() {
                        self.start_rip(ctx);
                    }
                    if self.rip.running {
                        let frac = if self.rip.total > 0 {
                            (self.rip.done as f32) / (self.rip.total as f32)
                        } else {
                            0.0
                        };
                        ui.add(egui::ProgressBar::new(frac).text(format!(
                            "帧 {} / {} · 候选 {} · {}%",
                            self.rip.done, self.rip.total, self.rip.found, self.rip.pct
                        )));
                    }
                });
                ui.separator();
                if let Some(r) = &self.rip.result {
                    let events = r
                        .get("events")
                        .and_then(|e| e.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    let elapsed = r.get("elapsed_s").and_then(|e| e.as_f64()).unwrap_or(0.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "抓取完成：字幕 {events} 条，耗时 {elapsed:.1}s"
                        ))
                        .strong(),
                    );
                    let mut items: Vec<(String, String)> = Vec::new();
                    if let Some(map) = r.get("artifacts").and_then(|a| a.as_object()) {
                        for (k, v) in map {
                            if let Some(p) = v.as_str() {
                                items.push((k.clone(), p.to_string()));
                            }
                        }
                    }
                    if let Some(proj) = r.get("project").and_then(|p| p.as_str()) {
                        items.push(("project".into(), proj.to_string()));
                    }
                    items.sort();
                    ui.horizontal(|ui| {
                        ui.label("产物：");
                        if ui.button("打开产物目录").clicked() {
                            let dir = if self.out_dir.trim().is_empty() {
                                self.video
                                    .as_ref()
                                    .map(|v| default_out_dir(&v.path))
                                    .unwrap_or_default()
                            } else {
                                self.out_dir.trim().to_string()
                            };
                            if !dir.is_empty() {
                                open_path(&dir);
                            }
                        }
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("artifacts_scroll")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            for (k, p) in items {
                                ui.horizontal(|ui| {
                                    if ui.small_button("打开").clicked() {
                                        open_path(&p);
                                    }
                                    ui.label(format!("{k}: {p}"));
                                });
                            }
                        });
                }
                if let Some(e) = &self.rip.error {
                    ui.label(
                        egui::RichText::new(format!("抓取错误：{e}")).color(egui::Color32::RED),
                    );
                }
            });
    }
}

impl EsrxpApp {
    fn open_video(&self, path: String, ctx: &egui::Context) {
        let Some(base) = self.base_url.clone() else {
            return;
        };
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let res =
                client::post_json(&base, "/api/video/open", &serde_json::json!({"path": path}))
                    .and_then(|v| {
                        parse_video_info(&v).ok_or_else(|| "视频信息解析失败".to_string())
                    });
            let _ = tx.send(Msg::VideoOpened(res));
            ctx2.request_repaint();
        });
    }

    fn open_project(&self, path: String, ctx: &egui::Context) {
        let Some(base) = self.base_url.clone() else {
            return;
        };
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let res = client::post_json(
                &base,
                "/api/project/open",
                &serde_json::json!({"path": path}),
            )
            .and_then(|v| {
                let video = parse_video_info(&v).ok_or_else(|| "工程缺少视频信息".to_string())?;
                let out_dir = v
                    .get("out_dir")
                    .and_then(|o| o.as_str())
                    .unwrap_or("")
                    .to_string();
                let cfg = v.get("config").cloned().unwrap_or(Value::Null);
                let config = serde_json::from_value::<UiConfig>(cfg).map_err(|e| e.to_string())?;
                Ok(ProjectInfo {
                    video,
                    out_dir,
                    config,
                })
            });
            let _ = tx.send(Msg::ProjectOpened(res));
            ctx2.request_repaint();
        });
    }

    fn save_default_config(&self, ctx: &egui::Context) {
        let Some(base) = self.base_url.clone() else {
            return;
        };
        let config = self.config.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let body = serde_json::json!({
                "config": serde_json::to_value(&config).unwrap_or(Value::Null)
            });
            let res = client::post_json(&base, "/api/config/file", &body);
            let _ = tx.send(Msg::ConfigLoaded(res.map(|_| config)));
            ctx2.request_repaint();
        });
    }

    fn load_default_config(&self, ctx: &egui::Context) {
        let Some(base) = self.base_url.clone() else {
            return;
        };
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let cfg = client::get_json(&base, "/api/config/default").and_then(|v| {
                let cfg_v = v.get("config").cloned().unwrap_or(Value::Null);
                serde_json::from_value::<UiConfig>(cfg_v).map_err(|e| e.to_string())
            });
            let _ = tx.send(Msg::ConfigLoaded(cfg));
            ctx2.request_repaint();
        });
    }
}

// ------------------------------------------------------------------ CJK 字体

/// 递归收集目录下的字体文件（含子目录；ttf/otf 优先，ttc 作为 Windows/macOS 兜底）。
fn collect_fonts(dir: &Path, depth: usize, out: &mut Vec<(String, std::path::PathBuf)>) {
    if depth > 5 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_fonts(&p, depth + 1, out);
            continue;
        }
        let ext = p
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "ttf" || ext == "otf" || ext == "ttc" {
            let name = p
                .file_name()
                .map(|f| f.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            out.push((name, p));
        }
    }
}

/// 加载系统 CJK 字体（最佳努力）：egui 默认字体不含中文，需补一个中文字体。
/// 跨平台扫描常见字体目录（递归），ttc 集合由 ab_glyph 取第一个 face 解析。
pub fn install_cjk_font(ctx: &egui::Context) {
    let mut candidates: Vec<(String, std::path::PathBuf)> = Vec::new();
    let scan_dirs: &[&str] = match std::env::consts::OS {
        "windows" => &[r"C:\Windows\Fonts"],
        "macos" => &[
            "/System/Library/Fonts",
            "/Library/Fonts",
            "/System/Library/Fonts/Supplemental",
        ],
        _ => &[
            "/usr/share/fonts",
            "/usr/local/share/fonts",
            "/usr/X11R6/lib/X11/fonts",
        ],
    };
    let hints = [
        "droid", "cjk", "wqy", "hei", "song", "yahei", "simsun", "fallback", "pingfang", "noto",
        "uming", "ukai",
    ];
    for d in scan_dirs {
        let mut found: Vec<(String, std::path::PathBuf)> = Vec::new();
        collect_fonts(Path::new(d), 0, &mut found);
        found.sort_by(|a, b| {
            let ra = hints
                .iter()
                .position(|h| a.0.contains(h))
                .unwrap_or(usize::MAX);
            let rb = hints
                .iter()
                .position(|h| b.0.contains(h))
                .unwrap_or(usize::MAX);
            // hint 优先级相同时，单字体 ttf/otf 优于 ttc 集合
            ra.cmp(&rb).then_with(|| {
                let a_ttc = a.0.ends_with(".ttc");
                let b_ttc = b.0.ends_with(".ttc");
                a_ttc.cmp(&b_ttc)
            })
        });
        candidates.extend(found);
    }
    let mut fonts = egui::FontDefinitions::default();
    for (_, p) in candidates {
        if let Ok(bytes) = std::fs::read(&p) {
            let fd = egui::FontData::from_owned(bytes);
            fonts
                .font_data
                .insert("cjk".to_owned(), std::sync::Arc::new(fd));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(fam)
                    .or_default()
                    .push("cjk".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}
