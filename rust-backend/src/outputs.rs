//! 输出模块 —— 对应 esrXP 的 .ssa / .idx+.sub / OCR 影像 / .esr 工程。
//!
//! - .ssa（ASS v4.00+）：mask 外轮廓（Moore 追踪 + RDP 简化）→ {\p1} 矢量绘图
//! - .idx/.sub（VobSub）：字幕位图 → 4bit RLE + YUV 调色板（标准格式）
//! - OCR 影像：白底黑字 PNG（image crate 编码）
//! - JSON 时间轴 + 现代工程文件

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::config::{AppConfig, OcrConfig};
use crate::ripper::SubtitleEvent;

// ---------------------------------------------------------------- 时间码
pub fn ass_time(t: f64) -> String {
    let t = t.max(0.0);
    let h = (t / 3600.0) as i64;
    let m = ((t % 3600.0) / 60.0) as i64;
    let s = t as i64 % 60;
    let cs = ((t - t.floor()) * 100.0).round() as i64;
    let cs = cs.min(99);
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

pub fn vobsub_time(t: f64) -> String {
    let t = t.max(0.0);
    let h = (t / 3600.0) as i64;
    let m = ((t % 3600.0) / 60.0) as i64;
    let s = t as i64 % 60;
    let ms = ((t - t.floor()) * 1000.0).round() as i64;
    let ms = ms.min(999);
    format!("{h:02}:{m:02}:{s:02}:{ms:03}")
}

fn shift(ev: &SubtitleEvent, shift_10ms: i64) -> (f64, f64) {
    let d = shift_10ms as f64 * 0.01;
    (ev.start + d, ev.end + d)
}

/// SRT 时间码：HH:MM:SS,mmm
pub fn srt_time(t: f64) -> String {
    let t = t.max(0.0);
    let h = (t / 3600.0) as i64;
    let m = ((t % 3600.0) / 60.0) as i64;
    let s = t as i64 % 60;
    let ms = ((t - t.floor()) * 1000.0).round() as i64;
    let ms = ms.min(999);
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// 写出 SRT —— 仅时间轴，不含文本。
/// 硬字幕是位图像素，无法可靠转录为文本；SRT 作为「字幕图片出现区间」的时间索引，
/// 配合 OCR 位图/SSA 使用。条目格式：序号 + start --> end + 空文本行。
pub fn write_srt(events: &[SubtitleEvent], cfg: &AppConfig, out: &Path) -> Result<()> {
    let st = &cfg.style;
    let mut s = String::new();
    for (i, ev) in events.iter().enumerate() {
        let (start, end) = shift(ev, st.time_shift_10ms);
        s.push_str(&format!(
            "{}
{} --> {}

",
            i + 1,
            srt_time(start),
            srt_time(end)
        ));
    }
    fs::write(out, s)?;
    Ok(())
}

// ---------------------------------------------------------------- SSA
pub fn write_ssa(
    events: &[SubtitleEvent],
    video_w: usize,
    video_h: usize,
    cfg: &AppConfig,
    out: &Path,
) -> Result<()> {
    let st = &cfg.style;
    let mut lines = String::new();
    lines.push_str("[Script Info]\nScriptType: v4.00+\n");
    lines.push_str(&format!("PlayResX: {video_w}\nPlayResY: {video_h}\n"));
    lines.push_str("WrapStyle: 0\nScaledBorderAndShadow: yes\n\n");
    lines.push_str("[V4+ Styles]\n");
    lines.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    let style_name = if st.no_default_style {
        "esrxpExtracted"
    } else {
        "Default"
    };
    lines.push_str(&format!(
        "Style: {style_name},{},36,{},{},{},{},0,0,0,0,100,100,0,0,1,{},{},2,10,10,10,1\n\n",
        st.font_name,
        st.primary_color,
        st.secondary_color,
        st.outline_color_ssa,
        st.shadow_color,
        st.outline_width,
        st.shadow_depth
    ));
    lines.push_str("[Events]\n");
    lines.push_str(
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );
    for ev in events {
        let (start, end) = shift(ev, st.time_shift_10ms);
        let (bx, by, _bw, _bh) = ev.bbox;
        let (ox, oy) = ev.roi_origin;
        let eps = if cfg.rip.better_quality { 0.5 } else { 1.2 };
        let drawing = mask_to_polygon(&ev.mask, ev.image_w, ev.image_h, eps);
        let text = if drawing.is_empty() {
            " ".to_string()
        } else {
            format!("{{\\an7}}{{\\pos({},{})}}{}", ox + bx, oy + by, drawing)
        };
        lines.push_str(&format!(
            "Dialogue: 0,{}, {},{},,0,0,0,,{}\n",
            ass_time(start),
            ass_time(end),
            style_name,
            text
        ));
    }
    fs::write(out, lines)?;
    Ok(())
}

/// mask → SSA 矢量绘图命令（外轮廓 + RDP 简化）。
/// `eps` 为 RDP 简化阈值：越小保留细节越多（Better Quality 时用 0.5）。
pub fn mask_to_polygon(mask: &[u8], w: usize, h: usize, eps: f64) -> String {
    let contours = trace_contours(mask, w, h);
    let mut parts: Vec<String> = Vec::new();
    for c in contours {
        if c.len() < 3 {
            continue;
        }
        let poly = rdp(&c, eps);
        if poly.len() < 3 {
            continue;
        }
        let first = poly[0];
        let mut cmd = format!("m {:.1} {:.1}", first.0 as f64, first.1 as f64);
        for (x, y) in &poly[1..] {
            cmd.push_str(&format!(" l {:.1} {:.1}", *x as f64, *y as f64));
        }
        cmd.push_str(&format!(" l {:.1} {:.1}", first.0 as f64, first.1 as f64));
        parts.push(cmd);
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("{{\\p1}}{}{{\\p0}}", parts.join(" "))
}

/// 连通域标签 + 逐行游程端点 → RDP 简化剪影轮廓（RETR_EXTERNAL 语义，鲁棒无死循环）。
fn trace_contours(mask: &[u8], w: usize, h: usize) -> Vec<Vec<(i64, i64)>> {
    let n = w * h;
    let mut labels = vec![0i32; n];
    let mut next: i32 = 1;
    // 8 连通 flood fill 打标签
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if mask[i] == 0 || labels[i] != 0 {
                continue;
            }
            labels[i] = next;
            let mut stack: Vec<(i64, i64)> = vec![(x as i64, y as i64)];
            while let Some((cx, cy)) = stack.pop() {
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (cx + dx, cy + dy);
                        if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                            let ni = ny as usize * w + nx as usize;
                            if mask[ni] > 0 && labels[ni] == 0 {
                                labels[ni] = next;
                                stack.push((nx, ny));
                            }
                        }
                    }
                }
            }
            next += 1;
        }
    }
    let ncomp = next;
    let mut out = Vec::new();
    for c in 1..ncomp {
        // 每行该域的 [first_x, last_x] 游程端点
        let mut lefts: Vec<(i64, i64)> = Vec::new();
        let mut rights: Vec<(i64, i64)> = Vec::new();
        for y in 0..h {
            let row = y * w;
            let mut first = -1i64;
            let mut last = -1i64;
            for x in 0..w {
                if labels[row + x] == c {
                    if first < 0 {
                        first = x as i64;
                    }
                    last = x as i64;
                }
            }
            if first >= 0 {
                lefts.push((first, y as i64));
                rights.push((last, y as i64));
            }
        }
        if lefts.len() < 3 {
            continue; // 过小组件
        }
        let l = rdp(&lefts, 0.5);
        let mut r = rdp(&rights, 0.5);
        r.reverse();
        let mut poly = l;
        poly.extend(r);
        if let (Some(f), Some(t)) = (poly.first().copied(), poly.last().copied()) {
            if f != t {
                poly.push(f); // 闭合
            }
        }
        if poly.len() >= 4 {
            out.push(poly);
        }
    }
    out
}

/// 8 邻域步进（Moore 轮廓追踪遗留，保留供轮廓算法调试复用）
#[allow(dead_code)]
fn step8(x: i64, y: i64, d: i64) -> (i64, i64) {
    const DX: [i64; 8] = [1, 1, 0, -1, -1, -1, 0, 1];
    const DY: [i64; 8] = [0, 1, 1, 1, 0, -1, -1, -1];
    (x + DX[d as usize], y + DY[d as usize])
}

/// Ramer–Douglas–Peucker 折线简化。
fn rdp(points: &[(i64, i64)], eps: f64) -> Vec<(i64, i64)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    fn perp_dist(p: (i64, i64), a: (i64, i64), b: (i64, i64)) -> f64 {
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len2 = (dx * dx + dy * dy) as f64;
        if len2 < 1e-9 {
            return (((p.0 - a.0).pow(2) + (p.1 - a.1).pow(2)) as f64).sqrt();
        }
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) as f64 / len2).clamp(0.0, 1.0);
        let (px, py) = (a.0 as f64 + t * dx as f64, a.1 as f64 + t * dy as f64);
        ((p.0 as f64 - px).powi(2) + (p.1 as f64 - py).powi(2)).sqrt()
    }
    let mut max_d = 0.0f64;
    let mut idx = 0usize;
    for i in 1..points.len() - 1 {
        let d = perp_dist(points[i], points[0], points[points.len() - 1]);
        if d > max_d {
            max_d = d;
            idx = i;
        }
    }
    if max_d > eps {
        let mut left = rdp(&points[..=idx], eps);
        let right = rdp(&points[idx..], eps);
        left.extend_from_slice(&right[1..]);
        left
    } else {
        vec![points[0], points[points.len() - 1]]
    }
}

// ---------------------------------------------------------------- VobSub
fn rgb_to_yuv(rgb: (u8, u8, u8)) -> (u8, u8, u8) {
    let (r, g, b) = (rgb.0 as f64, rgb.1 as f64, rgb.2 as f64);
    let y = (0.299 * r + 0.587 * g + 0.114 * b) as i64;
    let u = (-0.169 * r - 0.331 * g + 0.5 * b + 128.0) as i64;
    let v = (0.5 * r - 0.419 * g - 0.081 * b + 128.0) as i64;
    (
        y.clamp(0, 255) as u8,
        u.clamp(0, 255) as u8,
        v.clamp(0, 255) as u8,
    )
}

fn rle_encode_row(nibbles: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let n = nibbles.len();
    let mut i = 0;
    while i < n {
        let color = nibbles[i];
        let mut j = i;
        while j < n && nibbles[j] == color {
            j += 1;
        }
        let count = j - i;
        if count <= 15 {
            out.push(((count as u8) << 4) | color);
        } else if count <= 255 {
            out.push(color);
            out.push(count as u8);
        } else {
            out.push(color);
            out.push(0);
            out.push((count >> 8) as u8);
            out.push((count & 0xFF) as u8);
        }
        i = j;
    }
    out
}

pub fn write_vobsub(
    events: &[SubtitleEvent],
    video_w: usize,
    video_h: usize,
    cfg: &AppConfig,
    out_stem: &Path,
) -> Result<(String, String)> {
    let main = cfg.filter.subtitle_color;
    let outline = cfg.filter.outline_color;
    let st = &cfg.style;

    let mut sub: Vec<u8> = Vec::new();
    let mut offsets: Vec<(f64, f64, usize)> = Vec::new();

    for ev in events {
        if ev.mask.is_empty() || !ev.mask.iter().any(|v| *v > 0) {
            continue;
        }
        let (start, end) = shift(ev, st.time_shift_10ms);
        let offset = sub.len();
        let payload = encode_vobsub_frame(ev, main, outline);
        sub.extend_from_slice(&[0u8, 0, 0, 0]);
        sub.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        sub.extend_from_slice(&payload);
        offsets.push((start, end, offset));
    }

    let sub_path = out_stem.with_extension("sub");
    fs::write(&sub_path, &sub)?;

    // 对齐 esrXP 逆向取证：VobSub v7 模板 + 注释 + 16 项 YUV 调色板
    let mut idx = String::new();
    idx.push_str("# VobSub index file, v7 (do not modify this line!)\n");
    idx.push_str(&format!(
        "size: {video_w}x{video_h}\norg: 0, 0\nscale: 100%, 100%\n"
    ));
    idx.push_str("smooth: OFF\nfade: 0, 0\nalign: 0, 0\ntime offset: 0\nforced subs: OFF\n");
    idx.push_str("# Custom colors (transp idxs and the four colors)\n");
    idx.push_str("custom colors: OFF, tridx: 1110, colors: 0, 0, ffffff, 000000\n");
    let mut pal = Vec::new();
    for c in [outline, main, (255u8, 255u8, 255u8), (0u8, 0u8, 0u8)] {
        let (y, u, v) = rgb_to_yuv(c);
        pal.push(format!("{y:02x}{u:02x}{v:02x}"));
    }
    while pal.len() < 16 {
        pal.push("000000".to_string());
    }
    idx.push_str("# The original palette of the DVD in PGC#1\n");
    idx.push_str(&format!("palette: {}\n", pal.join(", ")));
    idx.push_str("langidx: 0\n");
    idx.push_str("# Decomment next line to activate alternative name in DirectVobSub / Windows Media Player 6.x\n");
    idx.push_str("# Force subtitle placement relative to (org.x, org.y)\n");
    idx.push_str("id: en, index: 0\n");
    for (start, _end, off) in &offsets {
        idx.push_str(&format!(
            "timestamp: {}, filepos: {:09x}\n",
            vobsub_time(*start),
            off
        ));
    }
    let idx_path = out_stem.with_extension("idx");
    fs::write(&idx_path, idx)?;
    Ok((
        sub_path.display().to_string(),
        idx_path.display().to_string(),
    ))
}

fn encode_vobsub_frame(ev: &SubtitleEvent, main: (u8, u8, u8), outline: (u8, u8, u8)) -> Vec<u8> {
    let (w, h) = (ev.image_w, ev.image_h);
    // 渲染前缀去噪点簇，避免 VobSub 字幕图椒盐噪声（与 OCR/位图一致）
    let mask = crate::postprocess::despeckle(&ev.mask, w, h, 4);
    let mut nib: Vec<u8> = vec![15; w * h]; // 默认透明
    for (i, px) in ev.image.chunks_exact(3).enumerate() {
        if mask[i] == 0 {
            continue;
        }
        let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
        let d_main = (r - main.0 as i32)
            .abs()
            .max((g - main.1 as i32).abs())
            .max((b - main.2 as i32).abs());
        let d_out = (r - outline.0 as i32)
            .abs()
            .max((g - outline.1 as i32).abs())
            .max((b - outline.2 as i32).abs());
        nib[i] = if d_out < d_main { 2 } else { 1 };
    }
    // 调色板（16 项 YUV+alpha）：0=黑,1=主色,2=描边
    let mut pal = Vec::with_capacity(64);
    for c in [(0u8, 0u8, 0u8), main, outline] {
        let (y, u, v) = rgb_to_yuv(c);
        pal.extend_from_slice(&[y, u, v, 0xFF]);
    }
    while pal.len() < 64 {
        pal.extend_from_slice(&[0, 0, 0, 0xFF]);
    }
    let mut rows: Vec<u8> = Vec::new();
    for y in 0..h {
        let row = &nib[y * w..y * w + w];
        let mut first: Option<usize> = None;
        let mut last: Option<usize> = None;
        for (x, v) in row.iter().enumerate() {
            if *v != 15 {
                if first.is_none() {
                    first = Some(x);
                }
                last = Some(x);
            }
        }
        let (fs, le) = match (first, last) {
            (Some(f), Some(l)) => (f, l),
            _ => continue,
        };
        rows.extend_from_slice(&(fs as u16).to_be_bytes());
        rows.extend_from_slice(&(le as u16).to_be_bytes());
        rows.extend_from_slice(&rle_encode_row(&row[fs..=le]));
    }
    let mut payload = pal;
    payload.extend_from_slice(&rows);
    payload
}

// ---------------------------------------------------------------- OCR PNG
/// OCR 影像 / 字幕截图导出（对齐 esrXP [FOCRImage]：Subtitle Per Image /
/// Scale Subtitle / Divid 2 lines；0.5.0 扩展渲染画质、后处理与输出格式）。
///
/// 画质要点：旧实现把二值 mask 按最近邻直接放大 → 边缘硬锯齿、放大后糊。
/// 新流程：
///   1. mask 双线性采样 + N×N 超采样 → 每个输出像素的「覆盖率」0..1（二值场 → 连续场）
///   2. 按覆盖率在前景 / 背景色之间混合 → 边缘连续灰度，平滑且不失真
///   3. color 模式取原视频像素做前景，保留硬字幕自身的高光与描边
pub fn write_ocr_png(
    events: &[SubtitleEvent],
    cfg: &AppConfig,
    out_dir: &Path,
) -> Result<Vec<String>> {
    let ocr = &cfg.output.ocr;
    let per_image = ocr.per_image.max(1) as usize;
    let ext = normalized_ext(&ocr.format);
    fs::create_dir_all(out_dir)?;
    let mut files = Vec::new();
    let mut img_idx = 0usize;
    let mut i = 0usize;
    while i < events.len() {
        let batch: Vec<&SubtitleEvent> = events[i..(i + per_image).min(events.len())]
            .iter()
            .filter(|ev| !ev.mask.is_empty() && ev.mask.iter().any(|v| *v > 0))
            .collect();
        if batch.is_empty() {
            i += per_image;
            continue;
        }
        img_idx += 1;
        let tiles: Vec<image::RgbaImage> = batch
            .iter()
            .filter_map(|ev| render_subtitle_tile(ev, ocr))
            .map(|t| apply_tile_postprocess(t, ocr))
            .collect();
        if tiles.is_empty() {
            i += per_image;
            continue;
        }
        let gap = 4u32;
        let total_w = tiles.iter().fold(gap, |acc, t| acc + t.width() + gap);
        let max_h = tiles.iter().map(|t| t.height()).max().unwrap_or(1);
        let bg = parse_hex_rgb(&ocr.bg_color, [255, 255, 255]);
        let mut img = image::RgbaImage::new(total_w.max(1), max_h.max(1));
        for p in img.pixels_mut() {
            *p = image::Rgba([bg[0], bg[1], bg[2], 255]);
        }
        let mut cx = gap;
        for t in &tiles {
            for y in 0..t.height() {
                for x in 0..t.width() {
                    if cx + x < img.width() {
                        img.put_pixel(cx + x, y, *t.get_pixel(x, y));
                    }
                }
            }
            cx += t.width() + gap;
        }
        let img = apply_canvas_postprocess(img, ocr);
        // Divid each subtitle into 2 lines：若存在整行空白带（两行字幕），拆成上下两张
        if ocr.divid_into_2_lines && max_h >= 12 {
            if let Some(split_y) = find_blank_split(&img, bg, 12) {
                let (top, bottom) = split_vertical(&img, split_y, bg);
                let pt = out_dir.join(format!("subtitle_{:04}_top.{ext}", img_idx));
                let pb = out_dir.join(format!("subtitle_{:04}_bottom.{ext}", img_idx));
                save_image(&top, &pt, &ext, ocr.quality)?;
                save_image(&bottom, &pb, &ext, ocr.quality)?;
                files.push(pt.display().to_string());
                files.push(pb.display().to_string());
                i += per_image;
                continue;
            }
        }
        let p = out_dir.join(format!("subtitle_{:04}.{ext}", img_idx));
        save_image(&img, &p, &ext, ocr.quality)?;
        crate::logging::debug(format!(
            "OCR 位图 {}x{} → {}",
            img.width(),
            img.height(),
            p.display()
        ));
        files.push(p.display().to_string());
        i += per_image;
    }
    // 清理上一次遗留的位图：本次写出数量减少（如字幕管理器里删了字幕）后，
    // 旧序号文件会留在目录里被误认为本次产物。逐个删掉超出本次编号范围的
    // subtitle_*.{ext}，仅限本函数自己生成的命名前缀，不碰用户其他文件。
    if let Ok(rd) = fs::read_dir(out_dir) {
        for e in rd.flatten() {
            let path = e.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(num) = ocr_stem_index(name) else {
                continue;
            };
            let keep = num <= img_idx;
            let matches_ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|e| e.eq_ignore_ascii_case(&ext))
                .unwrap_or(false);
            if !keep && matches_ext {
                let _ = fs::remove_file(&path);
            }
        }
    }
    Ok(files)
}

/// 从位图文件名解析序号：`subtitle_0003.png` / `subtitle_0004_top.jpg` → Some(3/4)。
/// 非本函数生成的命名（无 `subtitle_` 前缀、序号不可解析）返回 None，调用方据此跳过。
fn ocr_stem_index(name: &str) -> Option<usize> {
    name.strip_prefix("subtitle_")?
        .split('.')
        .next()?
        .split('_')
        .next()?
        .parse::<usize>()
        .ok()
}

/// 归一化扩展名：jpg/jpeg → jpg，bmp → bmp，其余一律 png（避免非法扩展名落到磁盘）。
fn normalized_ext(fmt: &str) -> String {
    let f = fmt.trim().trim_start_matches('.').to_ascii_lowercase();
    match f.as_str() {
        "jpg" | "jpeg" => "jpg".into(),
        "bmp" => "bmp".into(),
        _ => "png".into(),
    }
}

/// 解析 #RRGGBB 颜色；非法输入回退到默认色（避免单字符失误让整批导出失败）。
fn parse_hex_rgb(s: &str, default: [u8; 3]) -> [u8; 3] {
    let t = s.trim().trim_start_matches('#');
    if t.len() != 6 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return default;
    }
    let mut out = default;
    for i in 0..3usize {
        if let Ok(v) = u8::from_str_radix(&t[i * 2..i * 2 + 2], 16) {
            out[i] = v;
        }
    }
    out
}

/// mask 双线性采样：返回该点覆盖率 0.0..1.0。
/// 把二值 mask 变成连续场，任意缩放倍率都能得到平滑边缘（这是「不再有锯齿」的前提）。
fn sample_mask(mask: &[u8], w: usize, h: usize, fx: f64, fy: f64) -> f64 {
    if w == 0 || h == 0 || mask.len() < w * h {
        return 0.0;
    }
    let x = fx.clamp(0.0, w as f64 - 1.0);
    let y = fy.clamp(0.0, h as f64 - 1.0);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = x - x0 as f64;
    let ty = y - y0 as f64;
    let g = |ix: usize, iy: usize| -> f64 {
        if mask[iy * w + ix] > 0 {
            1.0
        } else {
            0.0
        }
    };
    let a = g(x0, y0) * (1.0 - tx) + g(x1, y0) * tx;
    let b = g(x0, y1) * (1.0 - tx) + g(x1, y1) * tx;
    a * (1.0 - ty) + b * ty
}

/// 原视频 RGB 双线性采样（color 模式取真实前景色时使用）。
fn sample_rgb(img: &[u8], w: usize, h: usize, fx: f64, fy: f64) -> (f64, f64, f64) {
    if w == 0 || h == 0 || img.len() < w * h * 3 {
        return (0.0, 0.0, 0.0);
    }
    let x = fx.clamp(0.0, w as f64 - 1.0);
    let y = fy.clamp(0.0, h as f64 - 1.0);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = x - x0 as f64;
    let ty = y - y0 as f64;
    let mut out = (0.0, 0.0, 0.0);
    for c in 0..3usize {
        let g = |ix: usize, iy: usize| -> f64 { img[(iy * w + ix) * 3 + c] as f64 };
        let a = g(x0, y0) * (1.0 - tx) + g(x1, y0) * tx;
        let b = g(x0, y1) * (1.0 - tx) + g(x1, y1) * tx;
        let v = a * (1.0 - ty) + b * ty;
        if c == 0 {
            out.0 = v;
        } else if c == 1 {
            out.1 = v;
        } else {
            out.2 = v;
        }
    }
    out
}

/// 由 ROI 原图 + 掩码现场构造一条字幕事件（供预览「导出效果」复用导出管线）。
/// 语义与抓取产出的 SubtitleEvent 一致：mask / image 均按命中区域 bbox 裁切。
pub fn event_from_roi(rgb: &[u8], rw: usize, rh: usize, mask: &[u8]) -> Option<SubtitleEvent> {
    if rw == 0 || rh == 0 || mask.len() < rw * rh || rgb.len() < rw * rh * 3 {
        return None;
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (rw as i64, rh as i64, -1i64, -1i64);
    for (i, v) in mask.iter().enumerate() {
        if *v > 0 {
            let x = (i % rw) as i64;
            let y = (i / rw) as i64;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if max_x < 0 || max_y < 0 {
        return None;
    }
    let (bw, bh) = (max_x - min_x + 1, max_y - min_y + 1);
    let mut sub_mask = vec![0u8; (bw * bh) as usize];
    let mut image = Vec::with_capacity((bw * bh * 3) as usize);
    for y in min_y..max_y + 1 {
        for x in min_x..max_x + 1 {
            let si = (y as usize) * rw + x as usize;
            sub_mask[((y - min_y) as usize) * bw as usize + (x - min_x) as usize] = mask[si];
            let ps = si * 3;
            image.extend_from_slice(&rgb[ps..ps + 3]);
        }
    }
    Some(SubtitleEvent {
        start: 0.0,
        end: 0.0,
        start_frame: 0,
        end_frame: 0,
        image,
        image_w: bw as usize,
        image_h: bh as usize,
        mask: sub_mask,
        roi_mask: mask.to_vec(),
        roi_w: rw,
        roi_h: rh,
        bbox: (min_x, min_y, bw, bh),
        roi_origin: (0, 0),
        diff_frames: 0,
        source_frame: 0,
        deleted: false,
    })
}

/// 单条字幕位图重建（含缩放）：以 mask 覆盖率驱动的抗锯齿合成取代二值放大。
pub fn render_subtitle_tile(ev: &SubtitleEvent, ocr: &OcrConfig) -> Option<image::RgbaImage> {
    let (sw, sh) = (ev.image_w, ev.image_h);
    if sw == 0 || sh == 0 {
        return None;
    }
    // 渲染前先除噪：移除面积 ≤ despeckle_min_area 的孤立噪点簇，
    // 消除 OCR / 位图缩略图上的椒盐噪声（clean 只处理单点/单线）。
    let packed = ocr.despeckle_min_area.max(0) as usize;
    let src_mask: std::borrow::Cow<[u8]> = if packed > 0 {
        std::borrow::Cow::Owned(crate::postprocess::despeckle(&ev.mask, sw, sh, packed))
    } else {
        std::borrow::Cow::Borrowed(&ev.mask)
    };
    let scale = ocr.scale.max(0.1);
    let dw = ((sw as f64) * scale).round().max(1.0) as usize;
    let dh = ((sh as f64) * scale).round().max(1.0) as usize;
    let fgc = parse_hex_rgb(&ocr.text_color, [0, 0, 0]);
    let bgc = parse_hex_rgb(&ocr.bg_color, [255, 255, 255]);
    let mode = ocr.color_mode.trim().to_ascii_lowercase();
    // color 模式需要真实像素；旧工程缺 image 时自动降级为前景/背景双色混合
    let use_pixels = mode == "color" && ev.image.len() >= sw * sh * 3;
    let ss = if ocr.antialias {
        ocr.supersample.clamp(1, 4) as usize
    } else {
        1usize
    };
    let step = 1.0 / ss as f64;
    let inv = step * step;
    // 第一趟：由二值 mask 重建「覆盖率场」（连续值 0..1，边缘天然软化）
    let mut cov = vec![0.0f64; dw * dh];
    let mut px: Option<Vec<[f64; 3]>> = if use_pixels {
        Some(vec![[0.0f64; 3]; dw * dh])
    } else {
        None
    };
    for y in 0..dh {
        for x in 0..dw {
            let mut c = 0.0f64;
            let mut acc = [0.0f64; 3];
            for sy in 0..ss {
                for sx in 0..ss {
                    let px_f = (x as f64 + (sx as f64 + 0.5) * step) / scale - 0.5;
                    let py_f = (y as f64 + (sy as f64 + 0.5) * step) / scale - 0.5;
                    c += sample_mask(&src_mask[..], sw, sh, px_f, py_f);
                    if use_pixels {
                        let (r, g, b) = sample_rgb(&ev.image, sw, sh, px_f, py_f);
                        acc[0] += r;
                        acc[1] += g;
                        acc[2] += b;
                    }
                }
            }
            cov[y * dw + x] = (c * inv).clamp(0.0, 1.0);
            if let Some(buf) = px.as_mut() {
                for k in 0..3 {
                    buf[y * dw + x][k] = acc[k] * inv;
                }
            }
        }
    }
    // 笔画加粗：对覆盖率场做最大值滤波（形态学膨胀），软边不被破坏，细笔画变实
    let rad = ocr.stroke_dilate.clamp(0, 4);
    let cov = if rad > 0 {
        let mut d = vec![0.0f64; dw * dh];
        for y in 0..dh {
            for x in 0..dw {
                let mut m = 0.0f64;
                for dy in -rad..=rad {
                    let ny = y as i64 + dy;
                    if ny < 0 || ny >= dh as i64 {
                        continue;
                    }
                    for dx in -rad..=rad {
                        let nx = x as i64 + dx;
                        if nx < 0 || nx >= dw as i64 {
                            continue;
                        }
                        let v = cov[ny as usize * dw + nx as usize];
                        if v > m {
                            m = v;
                        }
                    }
                }
                d[y * dw + x] = m;
            }
        }
        d
    } else {
        cov
    };
    let gamma = ocr.coverage_gamma.clamp(0.1, 4.0);
    let bin_th = ocr.binary_threshold.clamp(0.02, 0.98);
    let mut out = image::RgbaImage::new(dw as u32, dh as u32);
    for y in 0..dh {
        for x in 0..dw {
            let c = cov[y * dw + x];
            // binary 为二值硬边（阈值可调）；其余模式按覆盖率连续混合
            let alpha = if mode == "binary" {
                if c >= bin_th {
                    1.0
                } else {
                    0.0
                }
            } else if (gamma - 1.0).abs() < 1e-6 {
                c
            } else {
                c.powf(gamma).clamp(0.0, 1.0)
            };
            let (fr, fg, fb) = match px.as_ref() {
                Some(buf) => {
                    let p = buf[y * dw + x];
                    (p[0], p[1], p[2])
                }
                None => (fgc[0] as f64, fgc[1] as f64, fgc[2] as f64),
            };
            let mix = |c: f64, b: f64| -> u8 {
                (c * alpha + b * (1.0 - alpha)).clamp(0.0, 255.0).round() as u8
            };
            out.put_pixel(
                x as u32,
                y as u32,
                image::Rgba([
                    mix(fr, bgc[0] as f64),
                    mix(fg, bgc[1] as f64),
                    mix(fb, bgc[2] as f64),
                    255,
                ]),
            );
        }
    }
    Some(out)
}

/// 单条字幕后处理：裁剪 → 留边（单位均为输出像素，UI 所见即所得）。
pub fn apply_tile_postprocess(img: image::RgbaImage, ocr: &OcrConfig) -> image::RgbaImage {
    let (ct, cb, cl, cr) = (
        ocr.crop_top.max(0),
        ocr.crop_bottom.max(0),
        ocr.crop_left.max(0),
        ocr.crop_right.max(0),
    );
    let pad = ocr.padding.max(0) as u32;
    if ct == 0 && cb == 0 && cl == 0 && cr == 0 && pad == 0 {
        return img;
    }
    let (w, h) = (img.width() as i64, img.height() as i64);
    let x0 = cl.min(w.saturating_sub(1));
    let y0 = ct.min(h.saturating_sub(1));
    let cw = ((w - cr) - x0).max(1) as u32;
    let ch = ((h - cb) - y0).max(1) as u32;
    let mut cropped = image::RgbaImage::new(cw, ch);
    for y in 0..ch {
        for x in 0..cw {
            cropped.put_pixel(x, y, *img.get_pixel(x0 as u32 + x, y0 as u32 + y));
        }
    }
    if pad == 0 {
        return cropped;
    }
    let bg = parse_hex_rgb(&ocr.bg_color, [255, 255, 255]);
    let mut out = image::RgbaImage::new(cw + pad * 2, ch + pad * 2);
    for p in out.pixels_mut() {
        *p = image::Rgba([bg[0], bg[1], bg[2], 255]);
    }
    for y in 0..ch {
        for x in 0..cw {
            out.put_pixel(x + pad, y + pad, *cropped.get_pixel(x, y));
        }
    }
    out
}

/// 整图后处理：旋转 / 镜像 → 色调（亮度、对比度、灰度）→ 最大宽度限制。
pub fn apply_canvas_postprocess(img: image::RgbaImage, ocr: &OcrConfig) -> image::RgbaImage {
    let mut img = img;
    let rot = ((ocr.rotate % 360) + 360) % 360;
    img = match rot {
        90 => img_rotate_cw(&img),
        180 => img_rotate_180(&img),
        270 => img_rotate_ccw(&img),
        _ => img,
    };
    if ocr.flip_h {
        img = img_flip_h(&img);
    }
    if ocr.flip_v {
        img = img_flip_v(&img);
    }
    if ocr.brightness != 0 || ocr.contrast != 0 || ocr.grayscale {
        img = img_tone(&img, ocr.brightness, ocr.contrast, ocr.grayscale);
    }
    let mw = ocr.max_width.max(0) as u32;
    if mw > 0 && img.width() > mw {
        let nh = ((img.height() as f64) * (mw as f64 / img.width() as f64))
            .round()
            .max(1.0) as u32;
        img = image::imageops::resize(&img, mw, nh, filter_from_name(&ocr.scale_filter));
    }
    img
}

fn img_rotate_cw(img: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    let mut out = image::RgbaImage::new(h, w);
    for y in 0..h {
        for x in 0..w {
            out.put_pixel(h - 1 - y, x, *img.get_pixel(x, y));
        }
    }
    out
}

fn img_rotate_ccw(img: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    let mut out = image::RgbaImage::new(h, w);
    for y in 0..h {
        for x in 0..w {
            out.put_pixel(y, w - 1 - x, *img.get_pixel(x, y));
        }
    }
    out
}

fn img_rotate_180(img: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    let mut out = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            out.put_pixel(w - 1 - x, h - 1 - y, *img.get_pixel(x, y));
        }
    }
    out
}

fn img_flip_h(img: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    let mut out = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            out.put_pixel(w - 1 - x, y, *img.get_pixel(x, y));
        }
    }
    out
}

fn img_flip_v(img: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    let mut out = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            out.put_pixel(x, h - 1 - y, *img.get_pixel(x, y));
        }
    }
    out
}

/// 亮度 / 对比度 / 灰度：对比度采用标准系数 f = 259(c+255) / 255(259-c)。
fn img_tone(
    img: &image::RgbaImage,
    brightness: i64,
    contrast: i64,
    grayscale: bool,
) -> image::RgbaImage {
    let b = brightness.clamp(-100, 100) as f64;
    let c = (contrast.clamp(-100, 100) as f64) * 2.55;
    let f = (259.0 * (c + 255.0)) / (255.0 * (259.0 - c));
    let mut out = img.clone();
    for p in out.pixels_mut() {
        let (r0, g0, b0) = (p[0] as f64, p[1] as f64, p[2] as f64);
        let (mut r, mut g, mut bl) = (
            (f * (r0 - 128.0) + 128.0 + b).clamp(0.0, 255.0),
            (f * (g0 - 128.0) + 128.0 + b).clamp(0.0, 255.0),
            (f * (b0 - 128.0) + 128.0 + b).clamp(0.0, 255.0),
        );
        if grayscale {
            let l = 0.299 * r + 0.587 * g + 0.114 * bl;
            r = l;
            g = l;
            bl = l;
        }
        *p = image::Rgba([r.round() as u8, g.round() as u8, bl.round() as u8, p[3]]);
    }
    out
}

fn filter_from_name(name: &str) -> image::imageops::FilterType {
    match name.trim().to_ascii_lowercase().as_str() {
        "nearest" => image::imageops::FilterType::Nearest,
        "triangle" | "bilinear" => image::imageops::FilterType::Triangle,
        "catmullrom" | "bicubic" => image::imageops::FilterType::CatmullRom,
        "gaussian" => image::imageops::FilterType::Gaussian,
        _ => image::imageops::FilterType::Lanczos3,
    }
}

/// 按配置格式与质量写盘：PNG / BMP 无损（quality 忽略），JPG 走指定质量编码。
fn save_image(img: &image::RgbaImage, path: &Path, fmt: &str, quality: i64) -> Result<()> {
    if normalized_ext(fmt) == "jpg" {
        let q = quality.clamp(1, 100) as u8;
        // JpegEncoder 仅接受 L8 / Rgb8，故先去掉 alpha 通道
        let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
        let mut f = fs::File::create(path)?;
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, q);
        enc.encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )?;
    } else {
        img.save(path)?;
    }
    Ok(())
}

/// 找整行纯背景（空白）横带；返回中心 y（用于拆两行）。找不到返回 None。
/// 空白判定相对 `bg` 色做容差比较（支持深色背景，不再假定白底）。
fn find_blank_split(img: &image::RgbaImage, bg: [u8; 3], tol: u8) -> Option<u32> {
    let (w, h) = (img.width(), img.height());
    let tol = tol as i32;
    let mut blank_rows: Vec<u32> = Vec::new();
    for y in 0..h {
        let mut blank = true;
        for x in 0..w {
            let p = img.get_pixel(x, y);
            if (p[0] as i32 - bg[0] as i32).abs() > tol
                || (p[1] as i32 - bg[1] as i32).abs() > tol
                || (p[2] as i32 - bg[2] as i32).abs() > tol
            {
                blank = false;
                break;
            }
        }
        if blank {
            blank_rows.push(y);
        }
    }
    if blank_rows.len() < 3 {
        return None;
    }
    // 找最宽的连续空白带，中心位于全图 30%~70%（两行字幕的中缝）
    let mut best_len = 0usize;
    let mut best_start = 0u32;
    let mut i = 0usize;
    while i < blank_rows.len() {
        let mut j = i;
        while j + 1 < blank_rows.len() && blank_rows[j + 1] == blank_rows[j] + 1 {
            j += 1;
        }
        let len = j - i + 1;
        if len > best_len {
            best_len = len;
            best_start = blank_rows[i];
        }
        i = j + 1;
    }
    let mid = best_start + best_len as u32 / 2;
    if mid > h / 4 && mid < h * 3 / 4 {
        Some(mid)
    } else {
        None
    }
}

fn split_vertical(
    img: &image::RgbaImage,
    y: u32,
    bg: [u8; 3],
) -> (image::RgbaImage, image::RgbaImage) {
    let (w, h) = (img.width(), img.height());
    let mut top = image::RgbaImage::new(w, y.max(1));
    let mut bottom = image::RgbaImage::new(w, h.saturating_sub(y).max(1));
    for p in top.pixels_mut() {
        *p = image::Rgba([bg[0], bg[1], bg[2], 255]);
    }
    for p in bottom.pixels_mut() {
        *p = image::Rgba([bg[0], bg[1], bg[2], 255]);
    }
    for yy in 0..y.max(1) {
        for x in 0..w {
            *top.get_pixel_mut(x, yy) = *img.get_pixel(x, yy);
        }
    }
    for yy in 0..h.saturating_sub(y).max(1) {
        for x in 0..w {
            *bottom.get_pixel_mut(x, yy) = *img.get_pixel(x, y + yy);
        }
    }
    (top, bottom)
}

/// SRT + 位图（对齐 esrXP "SubRip with bitmap"）：写出纯时间轴 SRT，
/// 同时每字幕输出独立 .bmp（白底黑字，对应时间区间，不含文本）。
pub fn write_srt_bitmap(
    events: &[SubtitleEvent],
    cfg: &AppConfig,
    out: &Path,
) -> Result<(String, Vec<String>)> {
    write_srt(events, cfg, out)?;
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "subs".into());
    let dir = out
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{stem}_subs"));
    fs::create_dir_all(&dir)?;
    let mut files = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        if ev.mask.is_empty() || !ev.mask.iter().any(|v| *v > 0) {
            continue;
        }
        let ocr = &cfg.output.ocr;
        let tile = match render_subtitle_tile(ev, ocr) {
            Some(t) => t,
            None => continue,
        };
        // 文件名维持 .bmp：SubRip 位图约定，管内 Flag=Mem 引用外部文件；渲染质量由新模式保证
        let img = apply_canvas_postprocess(apply_tile_postprocess(tile, ocr), ocr);
        let p = dir.join(format!("{:05}.bmp", i + 1));
        save_image(&img, &p, "bmp", ocr.quality)?;
        files.push(p.display().to_string());
    }
    Ok((dir.display().to_string(), files))
}

// ---------------------------------------------------------------- JSON
pub fn write_json_timeline(
    events: &[SubtitleEvent],
    video_info: &serde_json::Value,
    cfg: &AppConfig,
    out: &Path,
) -> Result<()> {
    let st = &cfg.style;
    let subs: Vec<serde_json::Value> = events
        .iter()
        .enumerate()
        .map(|(i, ev)| {
            let (start, end) = shift(ev, st.time_shift_10ms);
            json!({
                "index": i + 1, "start": (start * 10000.0).round() / 10000.0,
                "end": (end * 10000.0).round() / 10000.0,
                "start_frame": ev.start_frame, "end_frame": ev.end_frame,
                "bbox": ev.bbox, "roi_origin": ev.roi_origin,
                "diff_frames": ev.diff_frames,
            })
        })
        .collect();
    let data = json!({
        "video": video_info, "style": {"time_shift_10ms": st.time_shift_10ms},
        "count": events.len(), "subtitles": subs,
    });
    fs::write(out, serde_json::to_string_pretty(&data)?)?;
    Ok(())
}

/// 不含 filtered 候选的工程写出（薄封装）。保留供 examples / 外部调用复用。
#[allow(dead_code)]
pub fn write_project(
    events: &[SubtitleEvent],
    video_path: &str,
    cfg: &AppConfig,
    out: &Path,
    artifacts: &serde_json::Value,
) -> Result<()> {
    write_esr(events, &[], video_path, cfg, out, artifacts)
}

/// .esr 工程文件（对齐 esrXP Save As .esr）：完整保存配置、字幕（含位图与
/// 删除标记）、被过滤候选，供字幕管理器打开/编辑/重导出。
pub fn write_esr(
    events: &[SubtitleEvent],
    filtered: &[SubtitleEvent],
    video_path: &str,
    cfg: &AppConfig,
    out: &Path,
    artifacts: &serde_json::Value,
) -> Result<()> {
    let subs: Vec<serde_json::Value> = events
        .iter()
        .enumerate()
        .map(|(i, ev)| ev_to_json(ev, i + 1))
        .collect();
    let flt: Vec<serde_json::Value> = filtered
        .iter()
        .enumerate()
        .map(|(i, ev)| ev_to_json(ev, i + 1))
        .collect();
    let data = json!({
        "format": "esrxp-ng/project", "version": 2,
        "video": video_path, "config": serde_json::to_value(cfg).unwrap_or(json!({})),
        "artifacts": artifacts, "filtered": flt, "subtitles": subs,
    });
    fs::write(out, serde_json::to_string_pretty(&data)?)?;
    Ok(())
}

fn ev_to_json(ev: &SubtitleEvent, index: usize) -> serde_json::Value {
    json!({
        "index": index, "start": ev.start, "end": ev.end,
        "start_frame": ev.start_frame, "end_frame": ev.end_frame,
        "bbox": ev.bbox, "roi_origin": ev.roi_origin,
        "roi_w": ev.roi_w, "roi_h": ev.roi_h,
        "image_w": ev.image_w, "image_h": ev.image_h,
        "diff_frames": ev.diff_frames, "source_frame": ev.source_frame,
        "deleted": ev.deleted,
        "image_b64": base64_encode(&ev.image),
        "mask_b64": base64_encode(&ev.mask),
        "roi_mask_b64": base64_encode(&ev.roi_mask),
    })
}

/// 读取 .esr 工程（v1/v2 兼容），返回 (events, filtered, video_path, config)。
pub fn load_esr(
    path: &Path,
) -> Result<(Vec<SubtitleEvent>, Vec<SubtitleEvent>, String, AppConfig)> {
    let data: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    let video = data["video"].as_str().unwrap_or("").to_string();
    let cfg: AppConfig = serde_json::from_value(data["config"].clone()).unwrap_or_default();
    let mut events = Vec::new();
    for s in data["subtitles"].as_array().cloned().unwrap_or_default() {
        let ev = json_to_ev(&s);
        events.push(ev);
    }
    let mut filtered = Vec::new();
    for s in data["filtered"].as_array().cloned().unwrap_or_default() {
        filtered.push(json_to_ev(&s));
    }
    Ok((events, filtered, video, cfg))
}

fn json_to_ev(s: &serde_json::Value) -> SubtitleEvent {
    let get = |k: &str, d: i64| s.get(k).and_then(|v| v.as_i64()).unwrap_or(d);
    let arr = |k: &str, i: usize| {
        s.get(k)
            .and_then(|v| v.as_array())
            .and_then(|a| a.get(i))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    };
    let image = base64_decode(s.get("image_b64").and_then(|v| v.as_str()).unwrap_or(""));
    let mask = base64_decode(s.get("mask_b64").and_then(|v| v.as_str()).unwrap_or(""));
    let roi_mask = base64_decode(s.get("roi_mask_b64").and_then(|v| v.as_str()).unwrap_or(""));
    SubtitleEvent {
        start: s.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0),
        end: s.get("end").and_then(|v| v.as_f64()).unwrap_or(0.0),
        start_frame: get("start_frame", 0),
        end_frame: get("end_frame", 0),
        image,
        mask,
        roi_mask,
        image_w: get("image_w", 0) as usize,
        image_h: get("image_h", 0) as usize,
        roi_w: get("roi_w", 0) as usize,
        roi_h: get("roi_h", 0) as usize,
        bbox: (
            arr("bbox", 0),
            arr("bbox", 1),
            arr("bbox", 2),
            arr("bbox", 3),
        ),
        roi_origin: (arr("roi_origin", 0), arr("roi_origin", 1)),
        diff_frames: get("diff_frames", 0),
        source_frame: get("source_frame", 0),
        deleted: s.get("deleted").and_then(|v| v.as_bool()).unwrap_or(false),
    }
}

// 极小 base64（无第三方依赖）
const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(s: &str) -> Vec<u8> {
    fn val(c: u8) -> u8 {
        match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    }
    let bytes: Vec<u8> = s
        .bytes()
        .filter(|b| *b != b'=' && *b != b'\n' && *b != b'\r')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let n = (val(chunk[0]) as u32) << 18
            | (chunk.get(1).map(|c| val(*c)).unwrap_or(0) as u32) << 12
            | (chunk.get(2).map(|c| val(*c)).unwrap_or(0) as u32) << 6
            | (chunk.get(3).map(|c| val(*c)).unwrap_or(0) as u32);
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧序号位图清理的解析基础：`subtitle_0003.png` 必须能解析出 3。
    /// 曾经的 bug 是只按 '_' 切分，拿到 "0003.png" 解析失败 → 清理静默跳过，
    /// 删除字幕后旧图仍留在目录里被误认为本次产物。
    #[test]
    fn ocr_stem_index_parses_plain_name() {
        assert_eq!(ocr_stem_index("subtitle_0003.png"), Some(3));
        assert_eq!(ocr_stem_index("subtitle_0012.jpg"), Some(12));
    }

    #[test]
    fn ocr_stem_index_parses_split_variants() {
        assert_eq!(ocr_stem_index("subtitle_0004_top.png"), Some(4));
        assert_eq!(ocr_stem_index("subtitle_0004_bottom.png"), Some(4));
    }

    /// 非本函数生成的文件必须返回 None，调用方据此跳过（不得误删用户文件）。
    #[test]
    fn ocr_stem_index_rejects_foreign_names() {
        assert_eq!(ocr_stem_index("other_0003.png"), None);
        assert_eq!(ocr_stem_index("subtitle_abc.png"), None);
        assert_eq!(ocr_stem_index("esrxp.log"), None);
        assert_eq!(ocr_stem_index("sample_hardsub.srt"), None);
    }
}
