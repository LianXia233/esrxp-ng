//! esrxp-ng-server —— Rust 后端（FFmpeg 解码 + 硬字幕抓取引擎 + axum HTTP API）。
//!
//! 用法：
//!   esrxp-ng-server rip <video> [--out DIR] [--config FILE]    # 引擎 CLI（验证/调试）
//!   esrxp-ng-server serve [--host 127.0.0.1] [--port 8000]      # 启动 UI 后端（HTTP，开发/浏览器用）
//!   esrxp-ng-server serve --pipe NAME                         # 管道模式（Electron 打包态，无端口）
//!   esrxp-ng-server dump-config                                 # 输出默认配置 JSON

mod api;
mod config;
mod filter;
mod gpu;
mod logging;
mod outputs;
mod postprocess;
mod ripper;
mod video;

use std::path::PathBuf;

use anyhow::Result;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_help();
        return Ok(());
    }
    match args[1].as_str() {
        "dbg" => cmd_dbg(&args[2..]),
        "dump-config" => {
            println!("{}", config::AppConfig::default_json());
            Ok(())
        }
        "rip" => cmd_rip(&args[2..]),
        "serve" => cmd_serve(&args[2..]),
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("未知命令: {other}");
            print_help();
            Ok(())
        }
    }
}

fn print_help() {
    println!(
        "esrxp-ng-server {} —— 硬字幕提取 Rust 后端\n\
         用法:\n\
         \x20 esrxp-ng-server rip <video> [--out DIR] [--config FILE]\n\
         \x20 esrxp-ng-server serve [--host H] [--port P] | serve --pipe NAME\n\
         \x20 esrxp-ng-server dump-config",
        env!("CARGO_PKG_VERSION")
    );
}

fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

/// 调试：解码前 3 帧打印像素统计与帧差（验证解码/拷贝正确性）。
fn cmd_dbg(args: &[String]) -> Result<()> {
    let video = args.first().cloned().ok_or_else(|| anyhow::anyhow!("缺少视频路径"))?;
    let mut vs = video::VideoSource::open(&video)?;
    println!("backend={}", vs.decode_backend.name());
    let mut prev: Option<Vec<u8>> = None;
    vs.decode_range(0, 3, 1, |fd| {
        let rgb = &fd.rgb;
        let n = rgb.len() / 3;
        // 统计
        let mut sum: u64 = 0;
        let mut minv = 255u8; let mut maxv = 0u8;
        for px in rgb.chunks_exact(3) {
            for &c in px { sum += c as u64; if c < minv { minv = c; } if c > maxv { maxv = c; } }
        }
        let mean = sum as f64 / (n * 3) as f64;
        println!("frame {}: n={} mean={:.1} min={} max={} first9={:?}",
                 fd.index, n, mean, minv, maxv, &rgb[0..9]);
        if let Some(ref p) = prev {
            let (changed, ratio, cmask) = filter::frame_diff_cpu(p, rgb, 24);
            println!("  vs prev: changed={} ratio={:.3}%", changed, ratio * 100.0);
            // 过滤统计
            let cfg = config::AppConfig::default();
            let raw = filter::filter_frame_cpu(rgb, fd.width, fd.height, &cfg.filter);
            let nz_raw = raw.iter().filter(|v| **v > 0).count();
            let clean = postprocess::clean(&raw, fd.width, fd.height, &cfg.postprocess);
            let nz_clean = clean.iter().filter(|v| **v > 0).count();
            // 自动选色尝试
            let (main, outline) = filter::auto_detect_colors(rgb, &cmask, fd.width, fd.height);
            println!("  filter: raw_nz={} clean_nz={}  auto(main={:?},out={:?}) plaus={}",
                     nz_raw, nz_clean, main, outline,
                     filter::colors_plausible(main, outline));
        }
        prev = Some(rgb.clone());
        Ok(())
    })?;
    Ok(())
}

pub fn cmd_rip(args: &[String]) -> Result<()> {
    let video = args.first().cloned().ok_or_else(|| anyhow::anyhow!("缺少视频路径"))?;
    let out = parse_flag(args, "--out").unwrap_or_else(|| "out".into());
    let config_path = parse_flag(args, "--config");

    // 尽早绑定工程日志：视频信息 / 进度 / 完成汇总都要落盘到 <工程目录>/esrxp.log
    let out_dir = PathBuf::from(&out);
    std::fs::create_dir_all(&out_dir)?;
    logging::bind_project(&out_dir);
    logging::info(format!("CLI 抓取开始: video={video} out={}", out_dir.display()));

    let cfg = match config_path {
        Some(p) => config::AppConfig::from_json(&std::fs::read_to_string(&p)?)?,
        None => config::AppConfig::default(),
    };
    let mut vs = video::VideoSource::open(&video)?;
    println!("视频: {}x{} @ {:.2} fps, {:.2}s, {} 帧",
             vs.width, vs.height, vs.fps, vs.duration, vs.frame_count);
    logging::info(format!("视频: {}x{} @ {:.2} fps, {:.2}s, {} 帧",
                          vs.width, vs.height, vs.fps, vs.duration, vs.frame_count));
    let res = ripper::rip(&mut vs, &cfg, {
        let mut last_pct: i64 = -1;
        move |done, total, found| {
            let pct = if total > 0 { done * 100 / total } else { 0 };
            if pct != last_pct && pct % 10 == 0 {
                last_pct = pct;
                logging::info(format!("进度 {pct}% ({done}/{total}) 候选 {found}"));
            }
            eprint!("\r  处理帧 {done}/{total}  候选 {found}");
        }
    })?;
    eprintln!();
    println!("完成：处理 {} 帧，变化帧 {}，字幕 {} 条，耗时 {:.2}s",
             res.frames_processed, res.candidates, res.events.len(), res.elapsed_s);
    logging::info(format!("抓取完成：处理 {} 帧，变化帧 {}，字幕 {} 条，耗时 {:.2}s",
                          res.frames_processed, res.candidates, res.events.len(), res.elapsed_s));

    let (mut artifacts, proj) = api::write_all_outputs(&res.events, &res.filtered, &video, &cfg, &out_dir)
        .map_err(|e| anyhow::Error::msg(e))?;
    artifacts.insert("project".into(), proj);
    for (k, v) in artifacts {
        println!("  {k}: {}", v);
    }
    println!("被过滤候选：{} 条（可用字幕管理器 Recover Filtered 恢复）", res.filtered.len());
    Ok(())
}

fn cmd_serve(args: &[String]) -> Result<()> {
    let pipe = parse_flag(args, "--pipe");
    let host = parse_flag(args, "--host").unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = parse_flag(args, "--port")
        .map(|p| p.parse().unwrap_or(8000))
        .unwrap_or(8000);
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ui_dir = parse_flag(args, "--ui").unwrap_or_else(|| {
        manifest.parent().unwrap_or(&manifest).join("ui").display().to_string()
    });
    let cache_dir = parse_flag(args, "--cache")
        .unwrap_or_else(|| std::env::temp_dir().join("esrxp-ng").display().to_string());
    std::fs::create_dir_all(&cache_dir)?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let app = api::router(ui_dir.clone(), cache_dir);
        match pipe {
            // 管道模式：Windows 命名管道 / Unix domain socket，Electron 打包态专用，不开网络端口
            Some(name) => {
                println!("esrxp-ng-server {} 已启动: pipe {name}  (UI: {ui_dir})",
                         env!("CARGO_PKG_VERSION"));
                serve_pipe(app, &name).await
            }
            None => {
                let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
                println!("esrxp-ng-server {} 已启动: http://{host}:{port}  (UI: {ui_dir})",
                         env!("CARGO_PKG_VERSION"));
                axum::serve(listener, app).await.map_err(|e| anyhow::Error::new(e))
            }
        }
    })?;
    Ok(())
}

/// 管道监听：每 accept 一个连接即以独立任务跑 HTTP/1.1，协议语义与 TCP 模式一致。
async fn serve_pipe(app: axum::Router, name: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use hyper_util::server::conn::auto::Builder as ConnBuilder;
        use hyper_util::service::TowerToHyperService;
        use tokio::net::windows::named_pipe::ServerOptions;
        let mut server = ServerOptions::new().first_pipe_instance(true).create(name)?;
        loop {
            server.connect().await?;
            let client = server; // 连接实例整体移交任务（tokio 命名管道无 try_clone）
            server = ServerOptions::new().create(name)?; // 先补位下一个实例，再 spawn 处理当前连接
            let svc = TowerToHyperService::new(app.clone()); // axum Router 是 tower Service，需适配 hyper Service
            tokio::spawn(async move {
                let _ = ConnBuilder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(TokioIo::new(client), svc).await;
            });
        }
    }
    #[cfg(not(windows))]
    {
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use hyper_util::server::conn::auto::Builder as ConnBuilder;
        use hyper_util::service::TowerToHyperService;
        let _ = std::fs::remove_file(name);
        let listener = tokio::net::UnixListener::bind(name)?;
        loop {
            let (sock, _) = listener.accept().await?;
            let svc = TowerToHyperService::new(app.clone());
            tokio::spawn(async move {
                let _ = ConnBuilder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(TokioIo::new(sock), svc).await;
            });
        }
    }
}
