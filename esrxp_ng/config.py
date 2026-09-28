"""配置模型 —— 对应逆向出的 esrXP 注册表 schema（HKCU\\software\\esrXP\\setting）。

esrXP 注册表键        →  现代配置字段
RipFrameSkip          →  rip.frame_skip
RipPixelDifference    →  rip.pixel_difference
RipIgnoreChange(%)    →  rip.ignore_change_percent
RipUp/Down/Left/Right →  region.up/down/left/right
RipScale / PreSharpen →  region.scale / region.sharpen
RipMethod             →  filter.method ("color" | "color_outline")
RipSubtitleColor      →  filter.subtitle_color (RGB)
RipOutlineColor       →  filter.outline_color (RGB)
Rip[Hue/RGB/LumMin/LumMax/SatMin/SatMax] + Enable × {Outline, Pass1, Final}
                      →  filter.segments.{outline,pass1,final}.*
RipPixelCompensate    →  filter.pixel_compensate
(后处理)              →  postprocess.{single_dot,single_line,large_block,touch_edge,pass_center}
StyleTimeShift(10ms)  →  style.time_shift_10ms
StyleUnicode          →  style.unicode
OCRSubtitlePreImage / OCRScaleSubtitle / OCRDividSubtitle
                      →  output.ocr.{per_image,scale,divid_into_2_lines}
"""
from __future__ import annotations

import json
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Any, Optional


@dataclass
class ColorSegment:
    """一段颜色判据：Hue 差 / RGB 差 / 亮度范围 / 饱和度范围，各通道可独立使能。"""

    hue: int = 0          # 目标色相 (0..179, OpenCV HSV 范围)
    hue_diff: int = 10    # 色相差值
    rgb: tuple = (255, 255, 255)   # 目标 RGB
    rgb_diff: int = 40    # RGB 差值（欧氏距离上限）
    lum_min: int = 0      # 最低亮度 (V 通道)
    lum_max: int = 255    # 最高亮度
    sat_min: int = 0      # 最低饱和度
    sat_max: int = 255    # 最高饱和度
    enable_hue: bool = False
    enable_rgb: bool = True
    enable_lum_min: bool = False
    enable_lum_max: bool = False
    enable_sat_min: bool = False
    enable_sat_max: bool = False

    def __post_init__(self):
        if not isinstance(self.rgb, tuple):
            self.rgb = tuple(self.rgb)


@dataclass
class RipConfig:
    frame_skip: int = 1            # 影像跳读：每 N 帧取 1 帧
    pixel_difference: int = 20     # 像点相差：变化像素数低于该值的帧忽略
    ignore_change_percent: float = 0.5  # 忽略改变(%)：变化像素占比低于该值的帧忽略
    diff_threshold: int = 24       # 单像素"变化"判定阈值（RGB 通道最大差）
    gap_frames: int = 5            # 字幕分段：候选帧间隔超过该帧数则断开为新字幕


@dataclass
class RegionConfig:
    up: int = 0
    down: int = 0
    left: int = 0
    right: int = 0
    scale: float = 1.0             # 放大影片（nearest/linear，>1 放大 ROI）
    sharpen: bool = False          # 锐化影片（unsharp mask）


@dataclass
class FilterConfig:
    method: str = "color_outline"  # "color"（仅字幕色）| "color_outline"（字幕色+边线）
    subtitle_color: tuple = (255, 255, 255)
    outline_color: tuple = (0, 0, 0)
    pixel_compensate: int = 1      # 像点补偿：mask 膨胀次数（弥补字体笔画缺口）
    segments: dict = field(default_factory=lambda: {
        "outline": ColorSegment(rgb=(0, 0, 0), enable_rgb=True, rgb_diff=30),
        "pass1": ColorSegment(rgb=(255, 255, 255), enable_rgb=True, rgb_diff=40),
        "final": ColorSegment(rgb=(255, 255, 255), enable_rgb=True, rgb_diff=40),
    })

    def __post_init__(self):
        if not isinstance(self.subtitle_color, tuple):
            self.subtitle_color = tuple(self.subtitle_color)
        if not isinstance(self.outline_color, tuple):
            self.outline_color = tuple(self.outline_color)
        if not isinstance(self.segments, dict):
            self.segments = {k: ColorSegment(**v) for k, v in self.segments.items()}


@dataclass
class PostprocessConfig:
    single_dot: bool = True        # 移除只有一像素的点
    single_line: bool = True       # 移除只有一像素的线
    large_block: int = 0           # 移除区块大过 N 像素（0=关闭）
    touch_edge: bool = False       # 移除接触边缘的区块
    pass_center: bool = True       # 移除通过中央的区块
    center_tolerance: float = 0.12 # 中央判定容差（区域高度的比例）


@dataclass
class StyleConfig:
    unicode: bool = True
    time_shift_10ms: int = 0       # 时间偏移，单位 10ms（对应 esrXP StyleTimeShift）
    outline_width: int = 1
    shadow_depth: int = 0
    primary_color: str = "&H00FFFFFF&"
    outline_color_ssa: str = "&H00000000&"


@dataclass
class OcrConfig:
    per_image: int = 1             # 每张影像含字幕（用于拼图；>=2 时分隔线）
    scale: float = 1.0             # 将字幕放大
    divid_into_2_lines: bool = False  # 将字幕分成两行


@dataclass
class OutputConfig:
    ssa: bool = True
    vobsub: bool = True
    ocr_png: bool = True
    json_timeline: bool = True
    ocr: OcrConfig = field(default_factory=OcrConfig)
    max_subtitle_width: int = 720  # 输出位图最大宽（超宽按比例缩）
    fps: Optional[float] = None    # 输出帧率（默认取视频源）


@dataclass
class AppConfig:
    rip: RipConfig = field(default_factory=RipConfig)
    region: RegionConfig = field(default_factory=RegionConfig)
    filter: FilterConfig = field(default_factory=FilterConfig)
    postprocess: PostprocessConfig = field(default_factory=PostprocessConfig)
    style: StyleConfig = field(default_factory=StyleConfig)
    output: OutputConfig = field(default_factory=OutputConfig)
    start_seconds: float = 0.0     # 抓取起点
    end_seconds: float = 0.0       # 抓取终点（0=到视频末尾）
    verbose: bool = False

    def dump(self) -> str:
        return json.dumps(asdict(self), ensure_ascii=False, indent=2)

    @classmethod
    def load(cls, path: str | Path) -> "AppConfig":
        data = json.loads(Path(path).read_text(encoding="utf-8"))
        return from_dict(data)

    def merge_overrides(self, overrides: dict[str, Any]) -> "AppConfig":
        """浅合并命令行覆盖项（键名如 'rip.frame_skip'）。"""
        cfg = from_dict(asdict(self))
        for key, value in overrides.items():
            parts = key.split(".")
            node: Any = cfg
            for p in parts[:-1]:
                node = getattr(node, p)
            setattr(node, parts[-1], value)
        return cfg


def from_dict(data: dict) -> AppConfig:
    d = json.loads(json.dumps(data))  # 深拷贝

    def seg(v: dict) -> ColorSegment:
        v = dict(v)
        if "rgb" in v and not isinstance(v["rgb"], tuple):
            v["rgb"] = tuple(v["rgb"])
        return ColorSegment(**v)

    f = d.get("filter", {})
    if "segments" in f:
        f["segments"] = {k: seg(v) for k, v in f["segments"].items()}
    o = d.get("output", {})
    if "ocr" in o:
        o["ocr"] = OcrConfig(**o["ocr"])

    cfg = AppConfig(
        rip=RipConfig(**d.get("rip", {})),
        region=RegionConfig(**d.get("region", {})),
        filter=FilterConfig(**f),
        postprocess=PostprocessConfig(**d.get("postprocess", {})),
        style=StyleConfig(**d.get("style", {})),
        output=OutputConfig(**o),
        start_seconds=d.get("start_seconds", 0.0),
        end_seconds=d.get("end_seconds", 0.0),
        verbose=d.get("verbose", False),
    )
    return cfg
