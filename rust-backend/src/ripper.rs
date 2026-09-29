//! 抓取引擎 —— 对应 esrXP 的 Rip 工作线程/主循环（mask 级状态机）。
//!
//! 流程：
//!   1. 逐帧（frame_skip 跳读）在裁切区域内做全帧帧差预筛（pixel_difference +
//!      ignore_change%），判定"字幕变化帧"；
//!   2. 对变化帧过滤 + 后处理得到字幕 mask（运算走 FrameKernels：CUDA→CPU）：
//!      - 出现     : 上一帧无字幕、当前有字幕 → 记录候选（开段）
//!      - 消失     : 上一帧有字幕、当前无字幕 → 记录封口帧，结束当前段
//!      - 内容变化 : 两帧都有字幕但 mask 内容差异显著 → 记录候选（开新段）
//!      静止显示期 mask 稳定 → 不重复记录；
//!   3. 按内容相似性 + gap_frames 分段（事件=出现帧→消失帧，完整覆盖显示期）；
//!   4. 内容近似且时间相邻的事件合并（合并重复字幕）。
//!   5. 自动选色带合理性门控（颜色判据 + mask 判据）。

use std::time::Instant;

use anyhow::Result;
use serde_json::json;

use crate::config::AppConfig;
use crate::filter::{auto_detect_colors, colors_plausible, mask_plausible};
use crate::gpu::{kernels, FrameKernels};
use crate::postprocess::clean;
use crate::video::{FrameData, VideoSource};

#[derive(Debug, Clone)]
pub struct SubtitleEvent {
    pub start: f64,
    pub end: f64,
    pub start_frame: i64,
    pub end_frame: i64,
    pub image: Vec<u8>,        // bbox 裁切 RGB24
    pub image_w: usize,
    pub image_h: usize,
    pub mask: Vec<u8>,         // 与 image 同尺寸 0/255
    pub roi_mask: Vec<u8>,     // 全 ROI 尺寸 0/255
    pub roi_w: usize,
    pub roi_h: usize,
    pub bbox: (i64, i64, i64, i64),   // (x, y, w, h) ROI 内
    pub roi_origin: (i64, i64),       // ROI 原点在原始帧中的坐标
    pub diff_frames: i64,
    pub source_frame: i64,
    pub deleted: bool,         // 字幕管理器：标记删除（Show/Hide Deleted / Purge）
}

#[derive(Debug, Clone)]
pub struct RipResult {
    pub events: Vec<SubtitleEvent>,
    pub filtered: Vec<SubtitleEvent>, // Recover Filtered：被过滤掉的候选段
    pub video_info: serde_json::Value,
    pub candidates: i64,
    pub frames_processed: i64,
    pub elapsed_s: f64,
    pub config: serde_json::Value,
}

#[derive(Clone)]
struct Candidate {
    idx: i64,
    time: f64,
    roi: Option<Vec<u8>>,      // 裁切后 RGB24（仅非空候选保存）
    mask: Vec<u8>,             // ROI 尺寸 0/255
    rw: usize,
    rh: usize,
    roi_origin: (i64, i64),    // ROI 左上角在原始帧中的坐标
    close_only: bool,
}

fn rgb_to_hue(rgb: (u8, u8, u8)) -> i64 {
    crate::filter::rgb_to_hsv(rgb.0, rgb.1, rgb.2).0
}

fn mask_iou(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut inter = 0i64;
    let mut union = 0i64;
    for i in 0..a.len() {
        let (pa, pb) = (a[i] > 0, b[i] > 0);
        if pa && pb {
            inter += 1;
        }
        if pa || pb {
            union += 1;
        }
    }
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

fn union_masks(frames: &[Candidate]) -> Vec<u8> {
    let mut u = vec![0u8; frames[0].mask.len()];
    for f in frames {
        for i in 0..u.len() {
            if f.mask[i] > 0 {
                u[i] = 255;
            }
        }
    }
    u
}

impl Candidate {
    fn roi_origin(&self) -> (i64, i64) {
        self.roi_origin
    }
}

fn union_masks2(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut u = a.to_vec();
    for i in 0..u.len() {
        if b[i] > 0 {
            u[i] = 255;
        }
    }
    u
}

/// 裁切 ROI + 缩放 + 锐化（缩放走 FrameKernels：CUDA→CPU）。
/// 对齐 esrXP Scale Video / Sharpen Video：preview.scale_video 乘入 ROI 缩放，
/// preview.sharpen_video 并入锐化。
pub fn prepare_roi(frame: &FrameData, cfg: &AppConfig) -> (Vec<u8>, usize, usize, (i64, i64)) {
    let (up, down, left, right) = (
        cfg.region.up.clamp(0, frame.height as i64) as usize,
        (cfg.region.down).clamp(0, frame.height as i64) as usize,
        (cfg.region.left).clamp(0, frame.width as i64) as usize,
        (cfg.region.right).clamp(0, frame.width as i64) as usize,
    );
    let (w, h) = (frame.width, frame.height);
    let y0 = up;
    let y1 = h.saturating_sub(down).max(y0);
    let x0 = left;
    let x1 = w.saturating_sub(right).max(x0);
    let rh = y1 - y0;
    let rw = x1 - x0;
    let mut roi = Vec::with_capacity(rw * rh * 3);
    for y in y0..y1 {
        let src = &frame.rgb[(y * w + x0) * 3..(y * w + x1) * 3];
        roi.extend_from_slice(src);
    }
    let mut roi2 = roi;
    let (rw, rh) = (rw, rh);
    let scale = cfg.region.scale * cfg.preview.scale_video.max(0.05);
    let sharpen = cfg.region.sharpen || cfg.preview.sharpen_video;
    // 缩放（最近邻；GPU/CPU 一致）
    if (scale - 1.0).abs() > 1e-6 && scale > 0.0 {
        let (scaled, nw, nh) = kernels().scale(&roi2, rw, rh, scale);
        roi2 = scaled;
        let (rw, rh) = (nw, nh);
        if sharpen {
            roi2 = sharpen3(&roi2, rw, rh);
        }
        return (roi2, rw, rh, (x0 as i64, y0 as i64));
    }
    if sharpen {
        roi2 = sharpen3(&roi2, rw, rh);
    }
    (roi2, rw, rh, (x0 as i64, y0 as i64))
}

fn sharpen3(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = rgb.to_vec();
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            for c in 0..3 {
                let center = rgb[(y * w + x) * 3 + c] as i32;
                let up = rgb[((y - 1) * w + x) * 3 + c] as i32;
                let down = rgb[((y + 1) * w + x) * 3 + c] as i32;
                let left = rgb[(y * w + x - 1) * 3 + c] as i32;
                let right = rgb[(y * w + x + 1) * 3 + c] as i32;
                // unsharp：v = center + 0.6 * (center - 邻域均值)
                let mean = (up + down + left + right) / 4;
                let v = (center + ((center - mean) * 3 / 5)).clamp(0, 255);
                out[(y * w + x) * 3 + c] = v as u8;
            }
        }
    }
    out
}

// ------------------------------------------------------------------ 主流程
pub fn rip<F>(video: &mut VideoSource, cfg: &AppConfig,
              mut on_progress: Option<F>) -> Result<RipResult>
where
    F: FnMut(i64, i64, i64),
{
    let t0 = Instant::now();
    let fps = video.fps;
    let total = if video.frame_count > 0 {
        video.frame_count
    } else {
        (video.duration * fps) as i64
    };
    let start_idx = (cfg.start_seconds * fps).round() as i64;
    let mut end_idx = if cfg.end_seconds > 0.0 {
        (cfg.end_seconds * fps).round() as i64
    } else {
        total
    };
    end_idx = end_idx.max(start_idx + 1).min(total.max(1));

    let k: &dyn FrameKernels = kernels();
    let mut prev_roi: Option<Vec<u8>> = None;
    let mut prev_mask: Option<Vec<u8>> = None;
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut filtered_candidates: Vec<Candidate> = Vec::new();
    let mut frames_done = 0i64;
    let mut diff_frames = 0i64;
    let mut color_tuned = false;

    let mut fcfg = cfg.filter.clone();
    let pcfg = cfg.postprocess.clone();
    let rcfg = cfg.rip.clone();

    let mut progress = on_progress.take();
    video.decode_range(start_idx, end_idx, rcfg.frame_skip, |fd| {
        frames_done += 1;
        let (roi, rw, rh, origin) = prepare_roi(&fd, cfg);

        if let Some(ref prev) = prev_roi {
            let (changed, ratio, changed_mask) = k.frame_diff(prev, &roi, rcfg.diff_threshold);
            let is_change = changed >= rcfg.pixel_difference
                && ratio * 100.0 >= rcfg.ignore_change_percent;
            if is_change {
                diff_frames += 1;

                let mut mask: Vec<u8> = Vec::new();
                if !color_tuned {
                    let (main, outline) = auto_detect_colors(&roi, &changed_mask, rw, rh);
                    if colors_plausible(main, outline) {
                        let saved = fcfg.clone();
                        apply_colors(&mut fcfg, main, outline);
                        let trial = clean(&k.filter(&roi, rw, rh, &fcfg), rw, rh, &pcfg);
                        if mask_plausible(&trial, rw * rh) {
                            mask = trial;
                            color_tuned = true;
                        } else {
                            fcfg = saved;
                        }
                    }
                }
                if mask.is_empty() {
                    mask = clean(&k.filter(&roi, rw, rh, &fcfg), rw, rh, &pcfg);
                }
                let cur_nonempty = mask.iter().any(|v| *v > 0);

                if cur_nonempty {
                    let content_changed = match &prev_mask {
                        Some(pm) => mask_iou(pm, &mask) < 0.7,
                        None => true,
                    };
                    if content_changed {
                        candidates.push(Candidate {
                            idx: fd.index,
                            time: fd.time,
                            roi: Some(roi.clone()),
                            mask: mask.clone(),
                            rw, rh,
                            roi_origin: origin,
                            close_only: false,
                        });
                    }
                    prev_mask = Some(mask);
                } else if prev_mask.is_some() {
                    candidates.push(Candidate {
                        idx: fd.index,
                        time: fd.time,
                        roi: None,
                        mask: Vec::new(),
                        rw, rh,
                        roi_origin: origin,
                        close_only: true,
                    });
                    prev_mask = None;
                } else if changed_mask.iter().any(|v| *v) {
                    // 有帧差区域但颜色过滤后为空 → 被过滤掉的候选（Recover Filtered）
                    let mask8: Vec<u8> = changed_mask.iter().map(|b| if *b { 255 } else { 0 }).collect();
                    filtered_candidates.push(Candidate {
                        idx: fd.index,
                        time: fd.time,
                        roi: Some(roi.clone()),
                        mask: mask8,
                        rw, rh,
                        roi_origin: origin,
                        close_only: false,
                    });
                }
            }
        }
        prev_roi = Some(roi);

        if let Some(ref mut p) = progress {
            if frames_done % fps.max(1.0) as i64 == 0 {
                p(frames_done, end_idx - start_idx, candidates.len() as i64);
            }
        }
        Ok(())
    })?;

    let events = segment(&candidates, fps, rcfg.gap_frames);
    let events = merge_repeat(events, fps, cfg.rip.force_merge);
    let filtered = segment(&filtered_candidates, fps, rcfg.gap_frames);

    let res = RipResult {
        events,
        filtered,
        video_info: json!({
            "path": video.path, "width": video.width, "height": video.height,
            "fps": video.fps, "duration_s": video.duration,
            "frame_count": video.frame_count, "codec": video.codec_name,
            "pix_fmt": video.pix_fmt, "decode_backend": video.decode_backend.name(),
        }),
        candidates: diff_frames,
        frames_processed: frames_done,
        elapsed_s: t0.elapsed().as_secs_f64(),
        config: serde_json::to_value(cfg).unwrap_or(json!({})),
    };
    Ok(res)
}

fn apply_colors(fcfg: &mut crate::config::FilterConfig, main: (u8, u8, u8), outline: (u8, u8, u8)) {
    fcfg.subtitle_color = main;
    fcfg.outline_color = outline;
    for name in ["pass1", "final"] {
        if let Some(s) = fcfg.segments.get_mut(name) {
            s.rgb = main;
            s.hue = rgb_to_hue(main);
            s.enable_rgb = true;
        }
    }
    if let Some(s) = fcfg.segments.get_mut("outline") {
        s.rgb = outline;
        s.hue = rgb_to_hue(outline);
        s.enable_rgb = true;
    }
}

// ------------------------------------------------------------------ 分段与合并
fn segment(candidates: &[Candidate], fps: f64, gap: i64) -> Vec<SubtitleEvent> {
    if candidates.is_empty() {
        return vec![];
    }
    let mut events: Vec<SubtitleEvent> = Vec::new();
    let mut cur: Vec<Candidate> = Vec::new();
    for c in candidates {
        if c.close_only {
            if !cur.is_empty() {
                events.push(make_event(&cur, fps, Some(c.time), Some(c.idx)));
                cur.clear();
            }
            continue;
        }
        if cur.is_empty() {
            cur.push(c.clone());
            continue;
        }
        let same = mask_iou(&union_masks(&cur), &c.mask) >= 0.7;
        let within_gap = c.idx - cur.last().unwrap().idx <= gap;
        if same || within_gap {
            cur.push(c.clone());
        } else {
            events.push(make_event(&cur, fps, None, None));
            cur.clear();
            cur.push(c.clone());
        }
    }
    if !cur.is_empty() {
        events.push(make_event(&cur, fps, None, None));
    }
    events
}

fn make_event(frames: &[Candidate], fps: f64,
              close_at: Option<f64>, close_frame: Option<i64>) -> SubtitleEvent {
    let first = &frames[0];
    let last = frames.iter().rev().find(|f| f.roi.is_some()).unwrap_or(&frames[0]);
    let frame_dur = 1.0 / fps;
    let end = close_at.unwrap_or(last.time + frame_dur);
    let end_frame = close_frame.unwrap_or(last.idx);
    let roi = last.roi.as_ref().unwrap();
    let (w, h) = (frames[0].rw, frames[0].rh);
    let mut roi_mask = vec![0u8; w * h];
    for f in frames {
        for i in 0..w * h {
            if f.mask[i] > 0 {
                roi_mask[i] = 255;
            }
        }
    }
    let mut min_x = w as i64;
    let mut min_y = h as i64;
    let mut max_x = 0i64;
    let mut max_y = 0i64;
    for (i, v) in roi_mask.iter().enumerate() {
        if *v > 0 {
            let x = (i % w) as i64;
            let y = (i / w) as i64;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    let (bx, by, bw, bh) = (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1);
    let mut mask = vec![0u8; (bw * bh) as usize];
    let mut image = Vec::with_capacity((bw * bh * 3) as usize);
    for y in by..by + bh {
        for x in bx..bx + bw {
            let src = ((y as usize) * w + x as usize);
            mask[((y - by) as usize * bw as usize + (x - bx) as usize)] = roi_mask[src];
            let ps = src * 3;
            image.extend_from_slice(&roi[ps..ps + 3]);
        }
    }
    SubtitleEvent {
        start: first.time,
        end,
        start_frame: first.idx,
        end_frame,
        image,
        image_w: bw as usize,
        image_h: bh as usize,
        mask,
        roi_mask,
        roi_w: w,
        roi_h: h,
        bbox: (bx, by, bw, bh),
        roi_origin: first.roi_origin(),
        diff_frames: frames.len() as i64,
        source_frame: last.idx,
        deleted: false,
    }
}

fn merge_repeat(events: Vec<SubtitleEvent>, fps: f64, force: bool) -> Vec<SubtitleEvent> {
    merge_repeat_with(events, force, 0.9, 2.0 / fps)
}

fn merge_repeat_with(events: Vec<SubtitleEvent>, force: bool, iou_threshold: f64, max_gap_time: f64) -> Vec<SubtitleEvent> {
    if events.is_empty() {
        return events;
    }
    let mut merged: Vec<SubtitleEvent> = vec![events[0].clone()];
    for ev in events.into_iter().skip(1) {
        let prev = merged.last_mut().unwrap();
        let iou = mask_iou(&prev.roi_mask, &ev.roi_mask);
        let gap_time = ev.start - prev.end;
        if force || (iou >= iou_threshold && gap_time <= max_gap_time) {
            prev.end = ev.end;
            prev.end_frame = ev.end_frame;
            prev.diff_frames += ev.diff_frames;
            prev.roi_mask = union_masks2(&prev.roi_mask, &ev.roi_mask);
            // 重算 bbox / mask / image（取时间更晚事件的图像）
            let (w, h) = (prev.roi_w, prev.roi_h);
            let mut min_x = w as i64;
            let mut min_y = h as i64;
            let mut max_x = 0i64;
            let mut max_y = 0i64;
            for (i, v) in prev.roi_mask.iter().enumerate() {
                if *v > 0 {
                    let x = (i % w) as i64;
                    let y = (i / w) as i64;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
            let (bx, by, bw, bh) = (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1);
            let mut mask = vec![0u8; (bw * bh) as usize];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    mask[((y - by) as usize * bw as usize + (x - bx) as usize)]
                        = prev.roi_mask[(y as usize) * w + x as usize];
                }
            }
            prev.bbox = (bx, by, bw, bh);
            prev.mask = mask;
            prev.image = ev.image.clone();
            prev.image_w = ev.image_w;
            prev.image_h = ev.image_h;
            prev.source_frame = ev.source_frame;
        } else {
            merged.push(ev);
        }
    }
    merged
}

/// 字幕管理器 Crop Subtitle：用当前 filter/后处理参数对某字幕时间段重新抓取，
/// 产出更干净的 bbox/image/mask（对齐 esrXP Crop 语义）。
/// 字幕管理器「合并重复」（对齐 esrXP MIMergeRepeat）：对事件列表按 mask 相似度一键合并。
/// 事件须已按 start 排序；返回 (合并后列表, 被合并掉条数)。
pub fn merge_repeat_manual(events: Vec<SubtitleEvent>, iou_threshold: f64, max_gap_s: f64) -> (Vec<SubtitleEvent>, usize) {
    let before = events.len();
    let merged = merge_repeat_with(events, false, iou_threshold, max_gap_s);
    (merged, before - merged.len())
}

pub fn crop_event(video: &mut VideoSource, cfg: &AppConfig, ev: &SubtitleEvent) -> Result<SubtitleEvent> {
    let k: &dyn FrameKernels = kernels();
    let pcfg = cfg.postprocess.clone();
    let fcfg = cfg.filter.clone();
    let mut cands: Vec<Candidate> = Vec::new();
    let start = (ev.start_frame as f64 / video.fps - 0.2).max(0.0) as i64;
    let end = (ev.end_frame + 1).min(video.frame_count.max(1));
    video.decode_range(start, end, 1, |fd| {
        let (roi, rw, rh, origin) = prepare_roi(&fd, cfg);
        let mask = clean(&k.filter(&roi, rw, rh, &fcfg), rw, rh, &pcfg);
        if mask.iter().any(|v| *v > 0) {
            cands.push(Candidate {
                idx: fd.index, time: fd.time,
                roi: Some(roi), mask, rw, rh, roi_origin: origin, close_only: false,
            });
        }
        Ok(())
    })?;
    if cands.is_empty() {
        anyhow::bail!("裁剪重抓无结果（可能区域/颜色设置与当前字幕不匹配）");
    }
    let mut made = make_event(&cands, video.fps, None, None);
    made.deleted = ev.deleted;
    made.start = ev.start;
    made.start_frame = ev.start_frame;
    Ok(made)
}
