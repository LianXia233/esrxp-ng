"""生成带硬字幕的合成测试视频（模拟动漫硬字幕：白色文字 + 黑色描边）。

用途：端到端验证 esrxp-ng 的 帧差判定 → 色彩过滤 → 后处理 → 字幕分段 → 输出。
背景为缓慢移动的彩色渐变 + 漂移方块（制造与字幕无关的帧间变化，考验 diff 判定）。
字幕 3 条（含中英文），白字黑边，位于画面下方。

用法：python scripts/make_sample_video.py [输出路径] [fps] [秒数]
"""
import sys
from fractions import Fraction
from pathlib import Path

import av
import numpy as np
from PIL import Image, ImageDraw, ImageFont

W, H = 1280, 720
FPS = 25


def subtitle_texts(total_s: float) -> list[tuple[float, float, str]]:
    return [
        (0.5, 1.5, "第一行字幕：你好世界"),
        (2.0, 3.0, "Second Subtitle Line"),
        (3.5, total_s - 0.3, "第三行字幕测试（硬字幕）"),
    ]


def draw_frame(t: float, font: ImageFont.FreeTypeFont) -> np.ndarray:
    img = Image.new("RGB", (W, H))
    dr = ImageDraw.Draw(img)
    # 移动的彩色渐变背景（每帧有可观变化，模拟动漫画面持续运动）
    for y in range(H):
        r = int(70 + 45 * np.sin(t * 3.0 + y / 55))
        g = int(70 + 45 * np.cos(t * 2.4 + y / 75))
        b = int(80 + 40 * np.sin(t * 1.8 + (y + 100) / 85))
        dr.line([(0, y), (W, y)], fill=(r, g, b))
    # 快速漂移方块（帧间变化干扰）
    bx = int((W * 0.7 + t * 420) % (W - 120))
    by = int((H * 0.35 + t * 260) % (H - 120))
    dr.rectangle([bx, by, bx + 110, by + 90], fill=(180, 90, 120))
    dr.ellipse([bx + 20, by + 15, bx + 90, by + 75], fill=(90, 180, 160))
    return np.asarray(img)


def draw_subtitle(img: np.ndarray, text: str, font: ImageFont.FreeTypeFont) -> np.ndarray:
    """白字 + 黑描边（模拟 anime 硬字幕），绘制在底部居中。"""
    img = Image.fromarray(img)
    dr = ImageDraw.Draw(img)
    bbox = dr.textbbox((0, 0), text, font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    x = (W - tw) // 2 - bbox[0]
    y = H - th - 60 - bbox[1]
    stroke = 4
    dr.text((x, y), text, font=font, fill=(255, 255, 255),
            stroke_width=stroke, stroke_fill=(0, 0, 0))
    return np.asarray(img)


def main(out_path: str = "sample_hardsub.mp4", fps: int = FPS, total_s: float = 5.0):
    fps = int(fps)
    total_s = float(total_s)
    out = Path(out_path)
    font_path = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"
    if not Path(font_path).exists():
        font_path = "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc"
    font = ImageFont.truetype(font_path, 46, index=0)
    subs = subtitle_texts(total_s)

    # 优先 libx264，回退 mpeg4
    container = av.open(str(out), "w")
    try:
        stream = container.add_stream("libx264", rate=fps)
    except Exception:
        stream = container.add_stream("mpeg4", rate=fps)
    stream.width, stream.height = W, H
    stream.pix_fmt = "yuv420p"
    stream.time_base = Fraction(1, fps)  # 显式时间基，避免 muxer 选择怪异的 tb

    n_frames = int(total_s * fps)
    for i in range(n_frames):
        t = i / fps
        frame = draw_frame(t, font)
        active = next((s for s0, s1, s in subs if s0 <= t < s1), None)
        if active:
            frame = draw_subtitle(frame, active, font)
        vf = av.VideoFrame.from_ndarray(frame, format="rgb24")
        vf.pts = i  # time_base = 1/fps，pts 即帧序号
        for packet in stream.encode(vf):
            container.mux(packet)
    for packet in stream.encode():
        container.mux(packet)
    container.close()
    print(f"已生成: {out}（{W}x{H}@{fps}fps, {total_s}s, 字幕 {len(subs)} 条）")


if __name__ == "__main__":
    main(*sys.argv[1:])
