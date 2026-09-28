//! esrxp-ng-server —— Rust 后端（FFmpeg 解码 + 硬字幕抓取引擎 + axum HTTP API）。
//!
//! 用法：
//!   esrxp-ng-server rip <video> [--out DIR] [--config FILE]    # 引擎 CLI（验证/调试）
//!   esrxp-ng-server serve [--host 127.0.0.1] [--port 8000]      # 启动 UI 后端
//!   esrxp-ng-server dump-config                                 # 输出默认配置 JSON

mod api;
mod config;
mod filter;
mod gpu;
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
         \x20 esrxp-ng-server serve [--host 127.0.0.1] [--port 8000]\n\
         \x20 esrxp-ng-server dump-config",
        env!("CARGO_PKG_VERSION")
    );
}

fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).map(|i| args[i + 1].clone())
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

fn cmd_rip(args: &[String]) -> Result<()> {
    let video = args.first().cloned().ok_or_else(|| anyhow::anyhow!("缺少视频路径"))?;
    let out = parse_flag(args, "--out").unwrap_or_else(|| "out".into());
    let config_path = parse_flag(args, "--config");

    let cfg = match config_path {
        Some(p) => config::AppConfig::from_json(&std::fs::read_to_string(&p)?)?,
        None => config::AppConfig::default(),
    };
    let mut vs = video::VideoSource::open(&video)?;
    println!("视频: {}x{} @ {:.2} fps, {:.2}s, {} 帧",
             vs.width, vs.height, vs.fps, vs.duration, vs.frame_count);
    let res = ripper::rip(&mut vs, &cfg, Some(|done, total, found| {
        eprint!("\r  处理帧 {done}/{total}  候选 {found}");
    }))?;
    eprintln!();
    println!("完成：处理 {} 帧，变化帧 {}，字幕 {} 条，耗时 {:.2}s",
             res.frames_processed, res.candidates, res.events.len(), res.elapsed_s);

    let out_dir = PathBuf::from(&out);
    std::fs::create_dir_all(&out_dir)?;
    let src = PathBuf::from(&video)
        .file_stem().map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "subtitle".into());
    let vinfo = &res.video_info;
    let (vw, vh) = (
        vinfo["width"].as_u64().unwrap_or(0) as usize,
        vinfo["height"].as_u64().unwrap_or(0) as usize,
    );
    let mut artifacts = std::collections::HashMap::new();
    if !res.events.is_empty() && cfg.output.ssa {
        let p = out_dir.join(format!("{src}.ass"));
        outputs::write_ssa(&res.events, vw, vh, &cfg, &p)?;
        artifacts.insert("ssa", p);
    }
    if !res.events.is_empty() && cfg.output.vobsub {
        let (sp, ip) = outputs::write_vobsub(&res.events, vw, vh, &cfg, &out_dir.join(&src))?;
        artifacts.insert("sub", PathBuf::from(sp));
        artifacts.insert("idx", PathBuf::from(ip));
    }
    if !res.events.is_empty() && cfg.output.ocr_png {
        let _files = outputs::write_ocr_png(&res.events, &cfg, &out_dir.join("subtitle_imgs"))?;
        artifacts.insert("ocr_images", out_dir.join("subtitle_imgs"));
    }
    if cfg.output.json_timeline {
        let p = out_dir.join(format!("{src}.timeline.json"));
        outputs::write_json_timeline(&res.events, vinfo, &cfg, &p)?;
        artifacts.insert("timeline", p);
    }
    let proj = out_dir.join(format!("{src}.esrng.json"));
    outputs::write_project(&res.events, &video, &cfg, &proj, &serde_json::json!(artifacts))?;
    artifacts.insert("project", proj);
    for (k, v) in artifacts {
        println!("  {k}: {}", v.display());
    }
    Ok(())
}

fn cmd_serve(args: &[String]) -> Result<()> {
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
        let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
        println!("esrxp-ng-server {} 已启动: http://{host}:{port}  (UI: {ui_dir})",
                 env!("CARGO_PKG_VERSION"));
        axum::serve(listener, app).await?;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}
