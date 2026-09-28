"""输出模块 —— 对应 esrXP 的 .esr / .srt / .ssa / .idx+.sub / OCR 影像导出。

现代化实现：
  - .ssa（ASS v4.00+）：mask 轮廓 → {\\p1} 矢量绘图（VSFilter/VLC 可渲染），文本行占位
  - .idx/.sub（VobSub）：字幕位图 → 4bit RLE + YUV 调色板（标准格式）
  - OCR 影像：每字幕裁剪 PNG（透明背景，可放大/分行）
  - JSON 时间轴 + 现代工程文件（.esrng.json，等价 .esr 可回读）
"""
from __future__ import annotations

import json
from pathlib import Path

import cv2
import numpy as np

from .config import AppConfig
from .ripper import SubtitleEvent


# ---------------------------------------------------------------- 时间码
def ass_time(t: float) -> str:
    t = max(t, 0.0)
    h = int(t // 3600)
    m = int((t % 3600) // 60)
    s = int(t % 60)
    cs = int(round((t - int(t)) * 100))
    if cs >= 100:
        cs = 99
    return f"{h}:{m:02d}:{s:02d}.{cs:02d}"


def vobsub_time(t: float) -> str:
    t = max(t, 0.0)
    h = int(t // 3600)
    m = int((t % 3600) // 60)
    s = int(t % 60)
    ms = int(round((t - int(t)) * 1000))
    if ms >= 1000:
        ms = 999
    return f"{h:02d}:{m:02d}:{s:02d}:{ms:03d}"


def _time_shift(event: SubtitleEvent, shift_10ms: int) -> tuple[float, float]:
    shift = shift_10ms * 0.01
    return event.start + shift, event.end + shift


# ---------------------------------------------------------------- SSA
def write_ssa(events: list[SubtitleEvent], video_w: int, video_h: int,
              cfg: AppConfig, out: Path):
    st = cfg.style
    lines = [
        "[Script Info]",
        "ScriptType: v4.00+",
        "PlayResX: %d" % video_w,
        "PlayResY: %d" % video_h,
        "WrapStyle: 0",
        "ScaledBorderAndShadow: yes",
        "",
        "[V4+ Styles]",
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, "
        "BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, "
        "BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding",
        "Style: Default,Arial,36,%s,&H000000FF&,%s,&H00000000&,0,0,0,0,100,100,0,0,1,"
        "%d,%d,2,10,10,10,1" % (st.primary_color, st.outline_color_ssa,
                                st.outline_width, st.shadow_depth),
        "",
        "[Events]",
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text",
    ]
    for ev in events:
        start, end = _time_shift(ev, st.time_shift_10ms)
        ox, oy = ev.roi_origin
        bx, by, bw, bh = ev.bbox
        drawing = _mask_to_polygon(ev.mask, bx, by)
        text = ""
        if drawing:
            text = r"{\an7}{\pos(%d,%d)}%s" % (ox + bx, oy + by, drawing)
        else:
            text = " "  # 空占位（mask 为空时）
        lines.append(f"Dialogue: 0,{ass_time(start)},{ass_time(end)},Default,,0,0,0,,{text}")
    out.write_text("\n".join(lines) + "\n", encoding="utf-8")


def _mask_to_polygon(mask: np.ndarray, bx: int, by: int) -> str:
    r"""mask → SSA 矢量绘图命令 {\p1}m ...{\p0}。坐标为相对 bbox 原点（\an7+\pos 定位）。"""
    if not mask.any():
        return ""
    contours, _ = cv2.findContours((mask > 0).astype(np.uint8), cv2.RETR_EXTERNAL,
                                   cv2.CHAIN_APPROX_SIMPLE)
    parts = []
    for c in contours:
        if len(c) < 3:
            continue
        poly = cv2.approxPolyDP(c, 0.7, True).reshape(-1, 2).astype(np.float32)
        if len(poly) < 3:
            continue
        cmd = f"m {poly[0][0]:.1f} {poly[0][1]:.1f}"
        segs = " ".join(f"l {x:.1f} {y:.1f}" for x, y in poly[1:])
        parts.append(cmd + " " + segs + " l %.1f %.1f" % (poly[0][0], poly[0][1]))
    if not parts:
        return ""
    return "{\\p1}" + " ".join(parts) + "{\\p0}"


# ---------------------------------------------------------------- VobSub
def _rgb_to_yuv(rgb) -> tuple[int, int, int]:
    r, g, b = (float(x) for x in rgb)
    y = int(0.299 * r + 0.587 * g + 0.114 * b)
    u = int(-0.169 * r - 0.331 * g + 0.5 * b + 128)
    v = int(0.5 * r - 0.419 * g - 0.081 * b + 128)
    return max(0, min(255, y)), max(0, min(255, u)), max(0, min(255, v))


def _classify_colors(image: np.ndarray, mask: np.ndarray, main: tuple, outline: tuple) -> np.ndarray:
    """把字幕区域像素分为 1=主色 2=描边 0=背景(透明)，输出 4bit 索引数组。"""
    h, w = mask.shape
    nib = np.full((h, w), 15, dtype=np.uint8)  # 默认透明
    m = mask > 0
    px = image[m].astype(np.float32)
    d_main = np.abs(px - np.asarray(main, np.float32)).max(axis=1)
    d_out = np.abs(px - np.asarray(outline, np.float32)).max(axis=1)
    is_outline = d_out < d_main
    nib_flat = np.where(is_outline, 2, 1).astype(np.uint8)
    nib[m] = nib_flat
    return nib


def _rle_encode_row(nibbles: np.ndarray) -> bytes:
    """对一行 4bit 索引（0..15）做 VobSub RLE。count 1..15 直接编码，>15 用扩展。"""
    out = bytearray()
    i = 0
    n = len(nibbles)
    while i < n:
        color = int(nibbles[i])
        j = i
        while j < n and int(nibbles[j]) == color:
            j += 1
        count = j - i
        if color == 0xF:
            # 透明游程（正常只出现在行内留白）
            pass
        if count <= 15:
            out.append((count << 4) | color)
        elif count <= 255:
            out.append(color)          # 高半字节 0 → 扩展
            out.append(count)
        else:
            out.append(color)
            out.append(0)
            out.append((count >> 8) & 0xFF)
            out.append(count & 0xFF)
        i = j
    return bytes(out)


def _encode_vobsub_frame(image: np.ndarray, mask: np.ndarray,
                         main: tuple, outline: tuple, frame_w: int) -> tuple[bytes, list[tuple[int, int]]]:
    """编码一个字幕帧的 .sub 数据块（调色板 + RLE 行），返回 (payload, [(row_y, x_start, x_end)])。"""
    nib = _classify_colors(image, mask, main, outline)
    h, w = nib.shape

    # 调色板（16 项，YUV + alpha）：0=背景(透明), 1=主色, 2=描边
    palette = []
    palette.append((0, 0, 0))                 # 0: 黑（透明）
    palette.append(_rgb_to_yuv(main))         # 1: 字幕主色
    palette.append(_rgb_to_yuv(outline))      # 2: 描边
    while len(palette) < 16:
        palette.append((0, 0, 0))
    pal_bytes = bytearray()
    for y, u, v in palette:
        pal_bytes += bytes((y, u, v, 0xFF))

    rows = bytearray()
    for y in range(h):
        row = nib[y]
        nz = np.nonzero(row != 15)[0]
        if len(nz) == 0:
            continue
        xs, xe = int(nz[0]), int(nz[-1])
        seg = row[xs:xe + 1]
        rows += int(xs).to_bytes(2, "big")
        rows += int(xe).to_bytes(2, "big")
        rows += _rle_encode_row(seg)

    payload = bytes(pal_bytes) + bytes(rows)
    return payload, []


def write_vobsub(events: list[SubtitleEvent], video_w: int, video_h: int,
                 cfg: AppConfig, out_stem: Path):
    """写出 .idx + .sub（VobSub 位图字幕）。out_stem 不含扩展名。"""
    fcfg = cfg.filter
    main, outline = fcfg.subtitle_color, fcfg.outline_color
    sub_path = out_stem.with_suffix(".sub")
    idx_path = out_stem.with_suffix(".idx")
    st = cfg.style

    sub = bytearray()
    offsets: list[tuple[float, float, int]] = []  # (start, end, offset)

    for ev in events:
        start, end = _time_shift(ev, st.time_shift_10ms)
        if not ev.mask.any():
            continue
        offset = len(sub)
        payload, _ = _encode_vobsub_frame(ev.image, ev.mask, main, outline, video_w)
        header = b"\x00\x00\x00\x00" + len(payload).to_bytes(4, "big")
        sub += header + payload
        offsets.append((start, end, offset))

    sub_path.write_bytes(bytes(sub))

    idx = ["v8", "# VobSub index file, v8 (do not modify this line!)",
           f"size: {video_w}x{video_h}", "org: 0, 0", "scale: 100%, 100%",
           "smooth: OFF", "fade: 0, 0", "align: 0, 0", "time offset: 0",
           "forced subs: OFF"]
    pal = []
    for c in [outline, main]:
        y, u, v = _rgb_to_yuv(c)
        pal.append(f"{y:02x}{u:02x}{v:02x}")
    while len(pal) < 16:
        pal.append("000000")
    idx.append("palette: " + ", ".join(pal))
    idx.append("langidx: 0")
    idx.append("id: en, index: 0")
    for start, end, off in offsets:
        idx.append(f"timestamp: {vobsub_time(start)}, filepos: {off:09x}")
    idx_path.write_text("\n".join(idx) + "\n", encoding="utf-8")
    return sub_path, idx_path


# ---------------------------------------------------------------- OCR PNG
def write_ocr_png(events: list[SubtitleEvent], cfg: AppConfig, out_dir: Path) -> list[Path]:
    ocr = cfg.output.ocr
    out_dir.mkdir(parents=True, exist_ok=True)
    files = []
    for i, ev in enumerate(events, 1):
        if not ev.mask.any():
            continue
        img = _compose_alpha(ev.image, ev.mask)
        if ocr.scale != 1.0 and ocr.scale > 0:
            img = cv2.resize(img, None, fx=ocr.scale, fy=ocr.scale,
                             interpolation=cv2.INTER_LANCZOS4)
        path = out_dir / f"subtitle_{i:04d}.png"
        cv2.imwrite(str(path), img)
        files.append(path)
    return files


def _compose_alpha(image: np.ndarray, mask: np.ndarray) -> np.ndarray:
    """字幕影像：白底黑字 BGRA（OCR 引擎友好，对应 esrXP 的 OCR 影像惯例）。"""
    out = np.full((*image.shape[:2], 3), 255, dtype=np.uint8)
    out[mask > 0] = (0, 0, 0)
    bgra = cv2.cvtColor(out, cv2.COLOR_BGR2BGRA)
    bgra[..., 3] = 255
    return bgra


# ---------------------------------------------------------------- JSON / 工程文件
def write_json_timeline(events: list[SubtitleEvent], video_info: dict,
                        cfg: AppConfig, out: Path):
    st = cfg.style
    data = {
        "video": video_info,
        "style": {"time_shift_10ms": st.time_shift_10ms},
        "count": len(events),
        "subtitles": [
            {
                "index": i,
                "start": round(_time_shift(ev, st.time_shift_10ms)[0], 4),
                "end": round(_time_shift(ev, st.time_shift_10ms)[1], 4),
                "start_frame": ev.start_frame,
                "end_frame": ev.end_frame,
                "bbox": list(ev.bbox),
                "roi_origin": list(ev.roi_origin),
                "diff_frames": ev.diff_frames,
            }
            for i, ev in enumerate(events, 1)
        ],
    }
    out.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")


def write_project(events: list[SubtitleEvent], video_path: str, cfg: AppConfig,
                  out: Path, artifacts: dict):
    """现代工程文件（等价 .esr）：配置 + 事件 + 产物引用，可回读重导出。"""
    data = {
        "format": "esrxp-ng/project",
        "version": 1,
        "video": video_path,
        "config": json.loads(cfg.dump()),
        "artifacts": {k: str(v) for k, v in artifacts.items()},
        "subtitles": [
            {
                "index": i,
                "start": ev.start,
                "end": ev.end,
                "start_frame": ev.start_frame,
                "end_frame": ev.end_frame,
                "bbox": list(ev.bbox),
            }
            for i, ev in enumerate(events, 1)
        ],
    }
    out.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
