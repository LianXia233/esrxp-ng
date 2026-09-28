"""esrxp-ng 命令行入口（typer，现代 CLI）。

用法示例：
  esrxp-ng info  video.mp4
  esrxp-ng rip   video.mp4 -o out --frame-skip 2 --ignore-change 1.0
  esrxp-ng dump-config > config.json
  esrxp-ng rip   video.mp4 -o out -c config.json --no-ssa
  esrxp-ng preview video.mp4 --frame 120 -o prev.png
"""
from __future__ import annotations

import json
import logging
import sys
import time
from pathlib import Path

import typer

from . import __version__
from .config import AppConfig
from .outputs import (write_json_timeline, write_ocr_png, write_project,
                      write_ssa, write_vobsub)
from .ripper import RipResult, rip
from .video import VideoSource

app = typer.Typer(add_completion=False, help="esrxp-ng —— 硬字幕提取（FFmpeg + numpy/OpenCV 现代化重构）")
logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")
log = logging.getLogger("esrxp-ng")


def _parse_rgb(value: str) -> tuple[int, int, int]:
    try:
        parts = [int(x) for x in value.replace("(", "").replace(")", "").split(",")]
        if len(parts) != 3:
            raise ValueError
        if not all(0 <= p <= 255 for p in parts):
            raise ValueError
        return tuple(parts)  # type: ignore[return-value]
    except ValueError:
        raise typer.BadParameter("RGB 格式应为 r,g,b（0-255）")


@app.command("info")
def info(video: str):
    """打印视频元数据（FFmpeg 解码探测）。"""
    with VideoSource(video) as vs:
        for k, v in vs.info().items():
            typer.echo(f"{k:12s}: {v}")


@app.command("dump-config")
def dump_config(out: str | None = typer.Option(None, "--out", "-o", help="写出默认配置 JSON（缺省打印到 stdout）")):
    """导出默认配置模板（对应 esrXP 注册表 schema 的现代 JSON 形态）。"""
    text = AppConfig().dump()
    if out:
        Path(out).write_text(text, encoding="utf-8")
        typer.echo(f"配置已写出: {out}")
    else:
        typer.echo(text)


@app.command("rip")
def rip_cmd(
    video: str,
    out: str = typer.Option("out", "--out", "-o", help="输出目录"),
    config: str | None = typer.Option(None, "--config", "-c", help="配置文件 JSON"),
    frame_skip: int | None = typer.Option(None, "--frame-skip", help="影像跳读（每 N 帧取 1 帧）"),
    pixel_difference: int | None = typer.Option(None, "--pixel-difference", help="像点相差阈值（变化像素数下限）"),
    ignore_change: float | None = typer.Option(None, "--ignore-change", help="忽略改变（变化占比下限）"),
    method: str | None = typer.Option(None, "--method", help='过滤方法: color | color_outline'),
    subtitle_color: str | None = typer.Option(None, "--subtitle-color", help="字幕色 r,g,b（关闭自动选色时生效）"),
    outline_color: str | None = typer.Option(None, "--outline-color", help="描边色 r,g,b"),
    no_auto_color: bool = typer.Option(False, "--no-auto-color", help="关闭自动选色"),
    start: float = typer.Option(0.0, "--start", help="抓取起点（秒）"),
    end: float = typer.Option(0.0, "--end", help="抓取终点（秒，0=到末尾）"),
    ssa: bool = typer.Option(True, "--ssa/--no-ssa", help="输出 .ssa"),
    vobsub: bool = typer.Option(True, "--vobsub/--no-vobsub", help="输出 .idx/.sub"),
    ocr_png: bool = typer.Option(True, "--png/--no-png", help="输出 OCR 字幕影像 PNG"),
    json_tl: bool = typer.Option(True, "--json/--no-json", help="输出 JSON 时间轴"),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="详细日志"),
):
    """抓取视频中的硬字幕，输出 .ssa / .idx+.sub / OCR PNG / JSON 时间轴。"""
    if verbose:
        logging.getLogger().setLevel(logging.DEBUG)

    cfg = AppConfig.load(config) if config else AppConfig()
    cfg.start_seconds = start
    cfg.end_seconds = end
    overrides: dict = {}
    if frame_skip is not None:
        overrides["rip.frame_skip"] = frame_skip
    if pixel_difference is not None:
        overrides["rip.pixel_difference"] = pixel_difference
    if ignore_change is not None:
        overrides["rip.ignore_change_percent"] = ignore_change
    if method is not None:
        if method not in ("color", "color_outline"):
            raise typer.BadParameter("method 必须为 color 或 color_outline")
        overrides["filter.method"] = method
    if subtitle_color is not None:
        overrides["filter.subtitle_color"] = _parse_rgb(subtitle_color)
        no_auto_color = True
    if outline_color is not None:
        overrides["filter.outline_color"] = _parse_rgb(outline_color)
        no_auto_color = True
    if overrides:
        cfg = cfg.merge_overrides(overrides)

    cfg.output.ssa = ssa
    cfg.output.vobsub = vobsub
    cfg.output.ocr_png = ocr_png
    cfg.output.json_timeline = json_tl

    out_dir = Path(out)
    out_dir.mkdir(parents=True, exist_ok=True)
    src_name = Path(video).stem

    def progress(done, total, found):
        if total > 0:
            pct = done * 100 // total
            sys.stdout.write(f"\r  处理帧 {done}/{total} ({pct}％)  候选变化帧 {found}")
            sys.stdout.flush()

    t0 = time.perf_counter()
    with VideoSource(video) as vs:
        typer.echo(f"视频: {vs.width}x{vs.height} @ {vs.frame_rate:.2f} fps, {vs.duration:.2f}s")
        typer.echo(f"抓取中（frame_skip={cfg.rip.frame_skip}, pixel_diff={cfg.rip.pixel_difference}, "
                   f"ignore_change={cfg.rip.ignore_change_percent:.2f}％）...")
        res: RipResult = rip(vs, cfg, on_progress=progress, auto_color=not no_auto_color)
        sys.stdout.write("\n")

    artifacts: dict = {}
    if res.events and cfg.output.ssa:
        p = out_dir / f"{src_name}.ass"
        write_ssa(res.events, res.video_info["width"], res.video_info["height"], cfg, p)
        artifacts["ssa"] = p
    if res.events and cfg.output.vobsub:
        sub_p, idx_p = write_vobsub(res.events, res.video_info["width"],
                                    res.video_info["height"], cfg, out_dir / src_name)
        artifacts["sub"] = sub_p
        artifacts["idx"] = idx_p
    if res.events and cfg.output.ocr_png:
        files = write_ocr_png(res.events, cfg, out_dir / "subtitle_imgs")
        artifacts["ocr_images"] = files
    if cfg.output.json_timeline:
        p = out_dir / f"{src_name}.timeline.json"
        write_json_timeline(res.events, res.video_info, cfg, p)
        artifacts["timeline"] = p

    # 现代工程文件（等价 .esr）
    proj = out_dir / f"{src_name}.esrng.json"
    write_project(res.events, video, cfg, proj, artifacts)
    artifacts["project"] = proj

    typer.echo("")
    typer.echo(f"完成：处理 {res.frames_processed} 帧，变化帧 {res.candidates}，"
               f"字幕 {len(res.events)} 条，耗时 {res.elapsed_s:.2f}s")
    for name, p in artifacts.items():
        if isinstance(p, list):
            typer.echo(f"  {name}: {len(p)} 个文件 -> {p[0].parent if p else '-'}")
        else:
            typer.echo(f"  {name}: {p}")


@app.command("preview")
def preview(
    video: str,
    frame: int = typer.Option(0, "--frame", "-f", help="帧序号"),
    out: str = typer.Option("preview.png", "--out", "-o", help="输出预览图"),
    config: str | None = typer.Option(None, "--config", "-c", help="配置文件"),
    region_only: bool = typer.Option(False, "--region-only", help="只显示裁切区域"),
):
    """输出单帧预览：左原帧、中过滤 mask、右叠加，便于肉眼调参。"""
    cfg = AppConfig.load(config) if config else AppConfig()
    import cv2
    import numpy as np
    from .filtering import filter_frame
    from .postprocess import clean
    from .ripper import _crop

    with VideoSource(video) as vs:
        fd = vs.frame_at(frame)
        full = fd.bgr
        roi, (ox, oy) = _crop(full, cfg.region.up, cfg.region.down,
                              cfg.region.left, cfg.region.right)
        mask = filter_frame(roi, cfg.filter)
        mask = clean(mask, cfg.postprocess)
        if region_only:
            base = roi
            offset = (0, 0)
        else:
            base = full
            offset = (ox, oy)
            # 在整帧上重建 mask
            m = np.zeros(full.shape[:2], dtype=np.uint8)
            m[oy:oy + roi.shape[0], ox:ox + roi.shape[1]] = mask
            mask = m

        overlay = base.copy()
        color = np.full(base.shape, (0, 255, 0), dtype=np.uint8)
        alpha = 0.55
        overlay = cv2.addWeighted(overlay, 1, color, alpha, 0)
        overlay[mask > 0] = base[mask > 0]  # 保留字幕像素原色
        panel = np.hstack([base, cv2.cvtColor(mask, cv2.COLOR_GRAY2BGR), overlay])
        cv2.imwrite(out, panel)
        typer.echo(f"预览已写出: {out}（左=原图 中=mask 右=叠加） 帧 {fd.index} @ {fd.time:.2f}s")


@app.command("version")
def version():
    """显示版本。"""
    typer.echo(f"esrxp-ng {__version__}")


if __name__ == "__main__":
    app()
