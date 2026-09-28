"""色彩过滤 —— 忠实还原 esrXP 的三段判据（Outline / Pass1 / Final）。

每段判据（对应注册表 Rip*Hue/RGB/LumMin/LumMax/SatMin/SatMax + Enable）：
  1. Hue 差   : 像素色相与目标色相的环形距离 <= hue_diff
  2. RGB 差   : 像素 RGB 与目标 RGB 的欧氏距离 <= rgb_diff
  3. 亮度范围 : lum_min <= V <= lum_max（HSV 的 V 通道）
  4. 饱和度范围: sat_min <= S <= sat_max
每通道独立使能，段内条件 AND，段之间 OR（任一段命中即为字幕像素）。

method = "color"        ：仅使用 final 段（+pass1 可选手动开启）
method = "color_outline"：outline 段（描边色）+ pass1/final 段（字幕本体色）合并

像素补偿（RipPixelCompensate）：mask 膨胀，弥合抗锯齿/压缩造成的笔画缺口。
实现：cv2.cvtColor HSV 一次 + numpy 向量化比较（RGB 用平方距离免 sqrt）+ cv2.dilate。
"""
from __future__ import annotations

import cv2
import numpy as np

from .config import ColorSegment, FilterConfig


def _hue_distance(h: np.ndarray, target: int) -> np.ndarray:
    """色相环形距离（OpenCV H 范围 0..179）。"""
    return np.minimum(np.abs(h - target), 180 - np.abs(h - target))


def segment_mask(h: np.ndarray, s: np.ndarray, v: np.ndarray, bgr: np.ndarray,
                 seg: ColorSegment) -> np.ndarray:
    """计算一段判据的命中 mask（HxW bool）。h/s/v 为预计算的 HSV 通道。"""
    mask = np.ones(bgr.shape[:2], dtype=bool)

    if seg.enable_hue:
        mask &= _hue_distance(h, seg.hue % 180) <= seg.hue_diff
    if seg.enable_rgb:
        t = np.asarray(seg.rgb, dtype=np.int32)
        d = bgr.astype(np.int32) - t
        dist2 = d[..., 0] * d[..., 0] + d[..., 1] * d[..., 1] + d[..., 2] * d[..., 2]
        mask &= dist2 <= seg.rgb_diff * seg.rgb_diff
    if seg.enable_lum_min:
        mask &= v >= seg.lum_min
    if seg.enable_lum_max:
        mask &= v <= seg.lum_max
    if seg.enable_sat_min:
        mask &= s >= seg.sat_min
    if seg.enable_sat_max:
        mask &= s <= seg.sat_max
    return mask


def _active_segments(fcfg: FilterConfig) -> list[ColorSegment]:
    segs = fcfg.segments
    if fcfg.method == "color_outline":
        return [segs["outline"], segs["final"]]
    if _enabled(segs["pass1"]) and not _enabled(segs["final"]):
        return [segs["pass1"]]
    return [segs["final"]]


def filter_frame(bgr: np.ndarray, fcfg: FilterConfig) -> np.ndarray:
    """对整个（已裁切）帧执行过滤，返回字幕像素 mask（uint8 0/255）。

    HSV 通道仅在启用 hue/lum/sat 判据时惰性计算（默认 RGB 判据无需 HSV）。
    """
    segs = _active_segments(fcfg)
    need_hsv = any(s.enable_hue or s.enable_lum_min or s.enable_lum_max
                   or s.enable_sat_min or s.enable_sat_max for s in segs)
    h = s = v = None
    if need_hsv:
        hsv = cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV)
        h, s, v = hsv[..., 0], hsv[..., 1], hsv[..., 2]
    combined = np.zeros(bgr.shape[:2], dtype=np.uint8)
    for seg in segs:
        m = segment_mask(h, s, v, bgr, seg).astype(np.uint8) * 255
        combined = cv2.bitwise_or(combined, m)

    # 像素补偿：膨胀
    n = max(0, int(fcfg.pixel_compensate))
    if n > 0:
        k = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (3, 3))
        for _ in range(n):
            combined = cv2.dilate(combined, k)
    return combined


def _enabled(seg: ColorSegment) -> bool:
    return any(
        [seg.enable_hue, seg.enable_rgb, seg.enable_lum_min, seg.enable_lum_max,
         seg.enable_sat_min, seg.enable_sat_max]
    )


def auto_detect_colors(frame_bgr: np.ndarray, diff_mask: np.ndarray,
                       k: int = 2) -> tuple[tuple[int, int, int], tuple[int, int, int]]:
    """自动估计字幕色与描边色。

    对变化区域像素按亮度排序：取最亮 15%（中位）作为字幕主色，最暗 15%（中位）
    作为描边色。白字黑边的动漫硬字幕在这种策略下不受背景渐变污染
    （背景亮度中等，被排除在两端之外）。对应 esrXP 用户手动"选色"的替代。
    """
    pts = frame_bgr[diff_mask > 0]
    if len(pts) < 16:
        return (255, 255, 255), (0, 0, 0)
    pts = pts.reshape(-1, 3).astype(np.int32)
    lum = pts.sum(axis=1)
    order = np.argsort(lum)
    n = len(order)
    hi = pts[order[int(n * 0.85):]]
    lo = pts[order[:max(1, int(n * 0.15))]]
    main = tuple(int(v) for v in np.median(hi, axis=0))
    outline = tuple(int(v) for v in np.median(lo, axis=0))
    return main, outline


def colors_plausible(main: tuple, outline: tuple) -> bool:
    """合理性门控：字幕主色应足够亮、描边应足够暗（区分于背景渐变的中间调）。"""
    return sum(main) / 3 >= 155 and sum(outline) / 3 <= 95


def mask_plausible(mask: np.ndarray, roi_area: int) -> bool:
    """mask 合理性：非空且不过度覆盖画面（防背景渗入）。mask 为 0/255。"""
    if not mask.any():
        return False
    return int((mask > 0).sum()) < roi_area * 0.30
