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

use crate::config::AppConfig;
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
pub fn write_ssa(events: &[SubtitleEvent], video_w: usize, video_h: usize,
                 cfg: &AppConfig, out: &Path) -> Result<()> {
    let st = &cfg.style;
    let mut lines = String::new();
    lines.push_str("[Script Info]\nScriptType: v4.00+\n");
    lines.push_str(&format!("PlayResX: {video_w}\nPlayResY: {video_h}\n"));
    lines.push_str("WrapStyle: 0\nScaledBorderAndShadow: yes\n\n");
    lines.push_str("[V4+ Styles]\n");
    lines.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    let style_name = if st.no_default_style { "esrxpExtracted" } else { "Default" };
    lines.push_str(&format!(
        "Style: {style_name},{},36,{},{},{},{},0,0,0,0,100,100,0,0,1,{},{},2,10,10,10,1\n\n",
        st.font_name, st.primary_color, st.secondary_color,
        st.outline_color_ssa, st.shadow_color, st.outline_width, st.shadow_depth));
    lines.push_str("[Events]\n");
    lines.push_str("Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
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
            ass_time(start), ass_time(end), style_name, text));
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
        let mut l = rdp(&lefts, 0.5);
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
        (((p.0 as f64 - px).powi(2) + (p.1 as f64 - py).powi(2))).sqrt()
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
    (y.clamp(0, 255) as u8, u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
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

pub fn write_vobsub(events: &[SubtitleEvent], video_w: usize, video_h: usize,
                    cfg: &AppConfig, out_stem: &Path) -> Result<(String, String)> {
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
    idx.push_str(&format!("size: {video_w}x{video_h}\norg: 0, 0\nscale: 100%, 100%\n"));
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
        idx.push_str(&format!("timestamp: {}, filepos: {:09x}\n", vobsub_time(*start), off));
    }
    let idx_path = out_stem.with_extension("idx");
    fs::write(&idx_path, idx)?;
    Ok((sub_path.display().to_string(), idx_path.display().to_string()))
}

fn encode_vobsub_frame(ev: &SubtitleEvent, main: (u8, u8, u8), outline: (u8, u8, u8)) -> Vec<u8> {
    let (w, h) = (ev.image_w, ev.image_h);
    let mut nib: Vec<u8> = vec![15; w * h]; // 默认透明
    for (i, px) in ev.image.chunks_exact(3).enumerate() {
        if ev.mask[i] == 0 {
            continue;
        }
        let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
        let d_main = (r - main.0 as i32).abs()
            .max((g - main.1 as i32).abs())
            .max((b - main.2 as i32).abs());
        let d_out = (r - outline.0 as i32).abs()
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
/// OCR 影像：白底黑字 PNG（对齐 esrXP [FOCRImage]：
/// Subtitle Per Image（每图字幕数）/ Scale Subtitle（放大）/ Divid 2 lines（拆两行））。
pub fn write_ocr_png(events: &[SubtitleEvent], cfg: &AppConfig, out_dir: &Path) -> Result<Vec<String>> {
    let ocr = &cfg.output.ocr;
    let scale = ocr.scale.max(0.1);
    let per_image = ocr.per_image.max(1) as usize;
    fs::create_dir_all(out_dir)?;
    let mut files = Vec::new();
    let mut img_idx = 0usize;
    let mut i = 0usize;
    while i < events.len() {
        let batch: Vec<&SubtitleEvent> = events[i..(i + per_image).min(events.len())]
            .iter().filter(|ev| !ev.mask.is_empty() && ev.mask.iter().any(|v| *v > 0))
            .collect();
        if batch.is_empty() {
            i += per_image;
            continue;
        }
        img_idx += 1;
        let gap = 4u32;
        let total_w = batch.iter().fold(gap, |acc, ev| {
            acc + (ev.image_w as f64 * scale).round() as u32 + gap
        });
        let max_h = batch.iter().map(|ev| {
            (ev.image_h as f64 * scale).round() as u32
        }).max().unwrap_or(1);
        let mut img = image::RgbaImage::new(total_w.max(1), max_h.max(1));
        for p in img.pixels_mut() {
            *p = image::Rgba([255, 255, 255, 255]);
        }
        let mut cx = gap;
        for ev in batch {
            let (w, h) = (ev.image_w, ev.image_h);
            let nw = (w as f64 * scale).round() as u32;
            let nh = (h as f64 * scale).round() as u32;
            for y in 0..nh {
                for x in 0..nw {
                    let sx = ((x as f64) / scale).floor() as usize;
                    let sy = ((y as f64) / scale).floor() as usize;
                    let si = sy.min(h - 1) * w + sx.min(w - 1);
                    let px = if ev.mask[si] > 0 {
                        image::Rgba([0, 0, 0, 255])
                    } else {
                        image::Rgba([255, 255, 255, 255])
                    };
                    img.put_pixel(cx + x, y, px);
                }
            }
            cx += nw + gap;
        }
        // Divid each subtitle into 2 lines：若存在整行空白带（两行字幕），拆成上下两张
        let p = out_dir.join(format!("subtitle_{:04}.png", img_idx));
        if ocr.divid_into_2_lines && max_h >= 12 {
            if let Some(split_y) = find_blank_split(&img) {
                let (top, bottom) = split_vertical(&img, split_y);
                let pt = out_dir.join(format!("subtitle_{:04}_top.png", img_idx));
                let pb = out_dir.join(format!("subtitle_{:04}_bottom.png", img_idx));
                top.save(&pt)?;
                bottom.save(&pb)?;
                files.push(pt.display().to_string());
                files.push(pb.display().to_string());
                i += per_image;
                continue;
            }
        }
        img.save(&p)?;
        files.push(p.display().to_string());
        i += per_image;
    }
    Ok(files)
}

/// 找整行全白（空白）横带；返回中心 y（用于拆两行）。找不到返回 None。
fn find_blank_split(img: &image::RgbaImage) -> Option<u32> {
    let (w, h) = (img.width(), img.height());
    let mut blank_rows: Vec<u32> = Vec::new();
    for y in 0..h {
        let mut blank = true;
        for x in 0..w {
            let p = img.get_pixel(x, y);
            if p[0] < 240 || p[1] < 240 || p[2] < 240 {
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

fn split_vertical(img: &image::RgbaImage, y: u32) -> (image::RgbaImage, image::RgbaImage) {
    let (w, h) = (img.width(), img.height());
    let mut top = image::RgbaImage::new(w, y.max(1));
    let mut bottom = image::RgbaImage::new(w, h.saturating_sub(y).max(1));
    for p in top.pixels_mut() {
        *p = image::Rgba([255, 255, 255, 255]);
    }
    for p in bottom.pixels_mut() {
        *p = image::Rgba([255, 255, 255, 255]);
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
pub fn write_srt_bitmap(events: &[SubtitleEvent], cfg: &AppConfig, out: &Path) -> Result<(String, Vec<String>)> {
    write_srt(events, cfg, out)?;
    let stem = out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "subs".into());
    let dir = out.parent().unwrap_or_else(|| Path::new(".")).join(format!("{stem}_subs"));
    fs::create_dir_all(&dir)?;
    let mut files = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        if ev.mask.is_empty() || !ev.mask.iter().any(|v| *v > 0) {
            continue;
        }
        let (w, h) = (ev.image_w, ev.image_h);
        let mut img = image::RgbaImage::new(w.max(1) as u32, h.max(1) as u32);
        for y in 0..h {
            for x in 0..w {
                let si = y * w + x;
                let px = if ev.mask[si] > 0 {
                    image::Rgba([0, 0, 0, 255])
                } else {
                    image::Rgba([255, 255, 255, 255])
                };
                img.put_pixel(x as u32, y as u32, px);
            }
        }
        let p = dir.join(format!("{:05}.bmp", i + 1));
        img.save(&p)?;
        files.push(p.display().to_string());
    }
    Ok((dir.display().to_string(), files))
}

// ---------------------------------------------------------------- JSON
pub fn write_json_timeline(events: &[SubtitleEvent], video_info: &serde_json::Value,
                           cfg: &AppConfig, out: &Path) -> Result<()> {
    let st = &cfg.style;
    let subs: Vec<serde_json::Value> = events.iter().enumerate().map(|(i, ev)| {
        let (start, end) = shift(ev, st.time_shift_10ms);
        json!({
            "index": i + 1, "start": (start * 10000.0).round() / 10000.0,
            "end": (end * 10000.0).round() / 10000.0,
            "start_frame": ev.start_frame, "end_frame": ev.end_frame,
            "bbox": ev.bbox, "roi_origin": ev.roi_origin,
            "diff_frames": ev.diff_frames,
        })
    }).collect();
    let data = json!({
        "video": video_info, "style": {"time_shift_10ms": st.time_shift_10ms},
        "count": events.len(), "subtitles": subs,
    });
    fs::write(out, serde_json::to_string_pretty(&data)?)?;
    Ok(())
}

pub fn write_project(events: &[SubtitleEvent], video_path: &str, cfg: &AppConfig,
                     out: &Path, artifacts: &serde_json::Value) -> Result<()> {
    write_esr(events, &[], video_path, cfg, out, artifacts)
}

/// .esr 工程文件（对齐 esrXP Save As .esr）：完整保存配置、字幕（含位图与
/// 删除标记）、被过滤候选，供字幕管理器打开/编辑/重导出。
pub fn write_esr(events: &[SubtitleEvent], filtered: &[SubtitleEvent], video_path: &str,
                 cfg: &AppConfig, out: &Path, artifacts: &serde_json::Value) -> Result<()> {
    let subs: Vec<serde_json::Value> = events.iter().enumerate().map(|(i, ev)| {
        ev_to_json(ev, i + 1)
    }).collect();
    let flt: Vec<serde_json::Value> = filtered.iter().enumerate().map(|(i, ev)| {
        ev_to_json(ev, i + 1)
    }).collect();
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
pub fn load_esr(path: &Path) -> Result<(Vec<SubtitleEvent>, Vec<SubtitleEvent>, String, AppConfig)> {
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
    let arr = |k: &str, i: usize| s.get(k)
        .and_then(|v| v.as_array())
        .and_then(|a| a.get(i))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let image = base64_decode(s.get("image_b64").and_then(|v| v.as_str()).unwrap_or(""));
    let mask = base64_decode(s.get("mask_b64").and_then(|v| v.as_str()).unwrap_or(""));
    let roi_mask = base64_decode(s.get("roi_mask_b64").and_then(|v| v.as_str()).unwrap_or(""));
    SubtitleEvent {
        start: s.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0),
        end: s.get("end").and_then(|v| v.as_f64()).unwrap_or(0.0),
        start_frame: get("start_frame", 0),
        end_frame: get("end_frame", 0),
        image, mask, roi_mask,
        image_w: get("image_w", 0) as usize,
        image_h: get("image_h", 0) as usize,
        roi_w: get("roi_w", 0) as usize,
        roi_h: get("roi_h", 0) as usize,
        bbox: (arr("bbox", 0), arr("bbox", 1), arr("bbox", 2), arr("bbox", 3)),
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
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
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
    let bytes: Vec<u8> = s.bytes().filter(|b| *b != b'=' && *b != b'\n' && *b != b'\r').collect();
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
