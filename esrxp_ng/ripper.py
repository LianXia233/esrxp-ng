"""抓取引擎 —— 对应 esrXP 的 Rip 工作线程/主循环（0x1294d0 / 0x129284）。

流程（mask 级状态机，忠实 esrXP 语义）：
  1. 逐帧（frame_skip 跳读）在裁切区域内做全帧帧差预筛（pixel_difference +
     ignore_change%），判定"字幕变化帧"；
  2. 对变化帧执行色彩过滤 + 后处理得到字幕 mask，按状态机处理：
     - 出现     : 上一帧无字幕、当前有字幕 → 记录候选（开段）
     - 消失     : 上一帧有字幕、当前无字幕 → 记录"封口帧"，结束当前段
     - 内容变化 : 两帧都有字幕但 mask 内容差异显著 → 记录候选（开新段）
     字幕静止显示期 mask 稳定 → 不重复记录（对应 esrXP 的 mask 游程帧差语义）；
  3. 按内容相似性 + gap_frames 把候选帧分段为字幕事件：
     事件从出现帧开始、到消失帧结束，静止显示期被完整覆盖；
  4. 内容近似（mask IoU 高）且时间相邻的事件合并（对应 esrXP "合并重复字幕"）。
  5. 自动选色带合理性门控：只有颜色判据（主色亮/描边暗）与 mask 判据
     （非空、不铺满画面）同时通过才生效，避免背景渐变污染。
"""
from __future__ import annotations

import logging
import time
from dataclasses import dataclass, field

import cv2
import numpy as np

from .config import AppConfig, FilterConfig
from .filtering import (auto_detect_colors, colors_plausible, filter_frame,
                        mask_plausible)
from .postprocess import clean
from .video import VideoSource

log = logging.getLogger("esrxp-ng.ripper")


@dataclass
class SubtitleEvent:
    start: float                  # 开始时间（秒）
    end: float                    # 结束时间（秒）
    start_frame: int
    end_frame: int
    image: np.ndarray             # 字幕位图（bbox 裁切，BGR）
    mask: np.ndarray              # 字幕 mask（与 image 同尺寸，0/255）
    roi_mask: np.ndarray          # 全 ROI 尺寸 mask（用于事件间 IoU 比较）
    bbox: tuple                   # (x, y, w, h)，ROI 内坐标
    roi_origin: tuple             # ROI 原点在原始帧中的坐标 (x, y)
    diff_frames: int = 0          # 支撑该事件的变化帧数
    source_frame: int = -1        # 取图来源帧号


@dataclass
class RipResult:
    events: list = field(default_factory=list)
    video_info: dict = field(default_factory=dict)
    candidates: int = 0           # 通过帧差判定的变化帧数
    frames_processed: int = 0
    elapsed_s: float = 0.0
    config: dict = field(default_factory=dict)


def _crop(frame: np.ndarray, up: int, down: int, left: int, right: int):
    h, w = frame.shape[:2]
    y0 = min(max(up, 0), h)
    y1 = max(h - max(down, 0), y0)
    x0 = min(max(left, 0), w)
    x1 = max(w - max(right, 0), x0)
    return frame[y0:y1, x0:x1], (x0, y0)


def _changed_stats(prev: np.ndarray, cur: np.ndarray, threshold: int) -> tuple[int, float, np.ndarray]:
    """返回 (变化像素数, 变化占比, 变化 mask)。任一路径通道差 > threshold 视为变化。"""
    d = np.abs(cur.astype(np.int16) - prev.astype(np.int16)).max(axis=2)
    changed_mask = (d > threshold)
    changed = int(changed_mask.sum())
    return changed, (changed / d.size if d.size else 0.0), changed_mask


def _prepare_roi(frame_bgr: np.ndarray, reg) -> tuple[np.ndarray, tuple[int, int]]:
    roi, (ox, oy) = _crop(frame_bgr, reg.up, reg.down, reg.left, reg.right)
    if reg.scale != 1.0 and reg.scale > 0:
        roi = cv2.resize(roi, None, fx=reg.scale, fy=reg.scale,
                         interpolation=cv2.INTER_LINEAR)
    if reg.sharpen:
        blur = cv2.GaussianBlur(roi, (0, 0), 1.2)
        roi = cv2.addWeighted(roi, 1.6, blur, -0.6, 0)
    return roi, (ox, oy)


# ---- 自动选色（带合理性门控） ------------------------------------------------
def _save_colors(fcfg: FilterConfig) -> dict:
    return {
        "sub": fcfg.subtitle_color, "out": fcfg.outline_color,
        "segs": {k: (s.rgb, s.hue, s.enable_rgb) for k, s in fcfg.segments.items()},
    }


def _apply_colors(fcfg: FilterConfig, main: tuple, outline: tuple):
    fcfg.subtitle_color = main
    fcfg.outline_color = outline
    for name in ("pass1", "final"):
        s = fcfg.segments[name]
        s.rgb = main
        s.hue = _rgb_to_hue(main)
        s.enable_rgb = True
    s = fcfg.segments["outline"]
    s.rgb = outline
    s.hue = _rgb_to_hue(outline)
    s.enable_rgb = True


def _restore_colors(fcfg: FilterConfig, saved: dict):
    fcfg.subtitle_color = saved["sub"]
    fcfg.outline_color = saved["out"]
    for k, (rgb, hue, en) in saved["segs"].items():
        fcfg.segments[k].rgb = rgb
        fcfg.segments[k].hue = hue
        fcfg.segments[k].enable_rgb = en


# ---- 主流程 ------------------------------------------------------------------
def rip(video: VideoSource, cfg: AppConfig,
        on_progress=None, auto_color: bool = True) -> RipResult:
    """执行抓取，返回字幕事件列表。"""
    t0 = time.perf_counter()
    rcfg, reg, fcfg, pcfg = cfg.rip, cfg.region, cfg.filter, cfg.postprocess
    fps = video.frame_rate
    total = video.frame_count if video.frame_count else int(video.duration * fps)
    start_idx = int(cfg.start_seconds * fps)
    end_idx = int(cfg.end_seconds * fps) if cfg.end_seconds > 0 else total
    end_idx = min(max(end_idx, start_idx + 1), max(total, 1))

    prev_roi = None
    prev_mask = None          # 上一保留帧的字幕 mask（ROI 尺寸；None=无字幕）
    candidates: list[dict] = []
    frames_done = 0
    diff_frames = 0
    color_tuned = False

    for fd in video.frames(start_idx, end_idx, rcfg.frame_skip):
        frames_done += 1
        roi, (ox, oy) = _prepare_roi(fd.bgr, reg)

        if prev_roi is None:
            prev_roi, prev_mask = roi, None
            continue

        changed, ratio, changed_mask = _changed_stats(prev_roi, roi, rcfg.diff_threshold)
        is_change = (changed >= rcfg.pixel_difference
                     and ratio * 100.0 >= rcfg.ignore_change_percent)
        prev_roi = roi
        if not is_change:
            continue
        diff_frames += 1

        mask = None
        if auto_color and not color_tuned:
            # 尝试自动选色：合理性门控通过才生效，否则沿用默认色并继续尝试
            main, outline = auto_detect_colors(roi, changed_mask)
            if colors_plausible(main, outline):
                saved = _save_colors(fcfg)
                _apply_colors(fcfg, main, outline)
                trial = clean(filter_frame(roi, fcfg), pcfg)
                if mask_plausible(trial, roi.shape[0] * roi.shape[1]):
                    mask = trial
                    color_tuned = True
                    log.info("自动选色生效: 字幕色=%s 描边色=%s", main, outline)
                else:
                    _restore_colors(fcfg, saved)
        if mask is None:
            mask = filter_frame(roi, fcfg)
            mask = clean(mask, pcfg)

        cur_nonempty = bool(mask.any())

        if cur_nonempty:
            # 内容变化才记录候选（静止显示期 mask 稳定 → 跳过，靠消失帧定 end）
            if prev_mask is None or _mask_iou(prev_mask, mask) < 0.7:
                candidates.append({
                    "idx": fd.index, "time": fd.time,
                    "roi": roi, "mask": mask, "origin": (ox, oy),
                })
            prev_mask = mask
        else:
            if prev_mask is not None:
                candidates.append({
                    "idx": fd.index, "time": fd.time,
                    "roi": roi, "mask": mask, "origin": (ox, oy),
                    "close_only": True,
                })
            prev_mask = None

        if on_progress and frames_done % max(1, int(fps)) == 0:
            on_progress(frames_done, end_idx - start_idx, len(candidates))

    events = _segment(candidates, fps, rcfg.gap_frames)
    events = _merge_repeat(events, fps, force=False)

    res = RipResult(
        events=events,
        video_info=video.info(),
        candidates=diff_frames,
        frames_processed=frames_done,
        elapsed_s=time.perf_counter() - t0,
        config=cfg.dump(),
    )
    return res


def _segment(candidates: list[dict], fps: float, gap: int) -> list[SubtitleEvent]:
    """分段：出现帧开段 → 内容变化（IoU 低）且超过 gap 则断段 → 封口帧闭段。"""
    if not candidates:
        return []
    events: list[SubtitleEvent] = []
    cur: list[dict] = []
    for c in candidates:
        if c.get("close_only"):
            if cur:
                events.append(_make_event(cur, fps, close_at=c["time"], close_frame=c["idx"]))
                cur = []
            continue
        if not cur:
            cur = [c]
            continue
        same = _mask_iou(_union_masks(cur), c["mask"]) >= 0.7
        within_gap = c["idx"] - cur[-1]["idx"] <= gap
        if same or within_gap:
            cur.append(c)
        else:
            events.append(_make_event(cur, fps))
            cur = [c]
    if cur:
        events.append(_make_event(cur, fps))
    return events


def _union_masks(frames: list[dict]) -> np.ndarray:
    union = np.zeros(frames[0]["mask"].shape, dtype=np.uint8)
    for f in frames:
        union = cv2.bitwise_or(union, f["mask"])
    return union


def _make_event(frames: list[dict], fps: float,
                close_at: float | None = None, close_frame: int | None = None) -> SubtitleEvent:
    first, last = frames[0], frames[-1]
    frame_dur = 1.0 / fps
    end = close_at if close_at is not None else last["time"] + frame_dur
    end_frame = close_frame if close_frame is not None else last["idx"]
    h, w = frames[0]["mask"].shape
    roi_mask = np.zeros((h, w), dtype=np.uint8)
    for f in frames:
        roi_mask = cv2.bitwise_or(roi_mask, f["mask"])
    ys, xs = np.nonzero(roi_mask)
    y0, y1, x0, x1 = ys.min(), ys.max() + 1, xs.min(), xs.max() + 1
    mask = roi_mask[y0:y1, x0:x1]
    image = last["roi"][y0:y1, x0:x1].copy()
    return SubtitleEvent(
        start=first["time"],
        end=end,
        start_frame=first["idx"],
        end_frame=end_frame,
        image=image,
        mask=mask,
        roi_mask=roi_mask,
        bbox=(int(x0), int(y0), int(x1 - x0), int(y1 - y0)),
        roi_origin=last["origin"],
        diff_frames=len(frames),
        source_frame=last["idx"],
    )


def _merge_repeat(events: list[SubtitleEvent], fps: float,
                  iou_threshold: float = 0.9, force: bool = False) -> list[SubtitleEvent]:
    """合并重复字幕：ROI 画布上 mask IoU 高且时间相邻的事件合并为一条（延伸 end）。"""
    if not events:
        return []
    merged = [events[0]]
    for ev in events[1:]:
        prev = merged[-1]
        iou = _mask_iou(prev.roi_mask, ev.roi_mask)
        gap_time = ev.start - prev.end
        if force or (iou >= iou_threshold and gap_time <= 2.0 / fps):
            prev.end = ev.end
            prev.end_frame = ev.end_frame
            prev.diff_frames += ev.diff_frames
            prev.roi_mask = cv2.bitwise_or(prev.roi_mask, ev.roi_mask)
            ys, xs = np.nonzero(prev.roi_mask)
            y0, y1, x0, x1 = ys.min(), ys.max() + 1, xs.min(), xs.max() + 1
            prev.bbox = (int(x0), int(y0), int(x1 - x0), int(y1 - y0))
            prev.mask = prev.roi_mask[y0:y1, x0:x1]
            prev.image = ev.image  # 时间更晚的图通常更完整
        else:
            merged.append(ev)
    return merged


def _mask_iou(a: np.ndarray, b: np.ndarray) -> float:
    if a.shape != b.shape or a.size == 0:
        return 0.0
    inter = int(((a > 0) & (b > 0)).sum())
    union = int(((a > 0) | (b > 0)).sum())
    return inter / union if union else 0.0


def _rgb_to_hue(rgb) -> int:
    r, g, b = (int(x) for x in rgb)
    hsv = cv2.cvtColor(np.uint8([[[b, g, r]]]), cv2.COLOR_BGR2HSV)
    return int(hsv[0, 0, 0])
