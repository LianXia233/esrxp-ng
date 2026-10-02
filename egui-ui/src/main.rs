//! esrxp-ng egui 前端（MVP）入口。
//!
//! 覆盖 MVP 区域：打开视频/工程 → 参数配置 → 预览 → 抓取进度 → 产物查看/保存。
//! 字幕管理器 / 批处理 / 日志面板留待后续迭代。
//!
//! 跨平台：Linux + Windows（macOS 低成本兼容）。后端以子进程方式随 GUI 启动，
//! GUI 通过 HTTP 127.0.0.1 与后端通信（后端崩溃不影响 GUI，可整体重启）。

mod app;
mod backend;
mod client;
mod config;
mod config_ui;

use app::EsrxpApp;
use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([960.0, 620.0])
            .with_title("esrxp-ng · 硬字幕提取"),
        ..Default::default()
    };
    eframe::run_native(
        "esrxp-ng",
        options,
        Box::new(|cc| {
            app::install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(EsrxpApp::new(cc)))
        }),
    )
}
