"""视频解码层 —— PyAV（FFmpeg 库绑定）替代 esrXP 的 DirectShow 管线。

提供：按帧索引随机读取（seek 到关键帧后前向解码）、帧率/时长/宽高元数据、
BGR24 ndarray 输出（与 OpenCV 像素管线直接对接）。
兼容容器：FFmpeg 支持的全部格式（mp4/mkv/avi/mov/rmvb/wmv/webm...）。
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import av
import numpy as np


@dataclass
class FrameData:
    index: int          # 帧序号（0 基）
    time: float         # 显示时间（秒）
    bgr: np.ndarray     # BGR24, HxWx3
    raw: object = None  # 原始 av.VideoFrame（保留引用避免缓冲回收）


class VideoSource:
    def __init__(self, path: str | Path):
        self.path = str(path)
        self.container = av.open(self.path)
        stream = next((s for s in self.container.streams if s.type == "video"), None)
        if stream is None:
            raise ValueError(f"'{self.path}' 中没有视频流")
        self.stream = stream
        self.width = stream.codec_context.width
        self.height = stream.codec_context.height
        self.time_base = float(stream.time_base)
        # h264 等格式下 average_rate/guessed_rate 常不可靠：先取值，异常则实测
        rate = float(stream.average_rate or stream.guessed_rate or 0)
        self.frame_rate = rate if 1 <= rate <= 1000 else self._measure_fps()
        # 时长：优先 container.duration（微秒），异常则退回 stream.duration*time_base
        container_us = float(self.container.duration or 0)
        dur_from_container = container_us / 1_000_000.0
        dur_from_stream = float(stream.duration or 0) * self.time_base
        if 0 < dur_from_container < 1e5:
            self.duration = dur_from_container
        elif 0 < dur_from_stream < 1e5:
            self.duration = dur_from_stream
        else:
            self.duration = 0.0
        self.frame_count = int(stream.frames or 0)
        if self.frame_count <= 0:
            if self.duration > 0:
                self.frame_count = int(round(self.duration * self.frame_rate))
        elif self.duration <= 0:
            self.duration = self.frame_count / self.frame_rate

    def _measure_fps(self) -> float:
        """实测帧率：解码前若干帧，取 pts 间隔中位数。"""
        deltas = []
        last = None
        self.container.seek(0)
        for frame in self.container.decode(self.stream):
            if frame.pts is None:
                continue
            if last is not None:
                d = (frame.pts - last) * self.time_base
                if d > 0:
                    deltas.append(d)
            last = frame.pts
            if len(deltas) >= 12:
                break
        if deltas:
            med = sorted(deltas)[len(deltas) // 2]
            if med > 0:
                return 1.0 / med
        return 25.0

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def close(self):
        try:
            self.container.close()
        except Exception:
            pass

    def info(self) -> dict:
        return {
            "path": self.path,
            "width": self.width,
            "height": self.height,
            "fps": round(self.frame_rate, 4),
            "duration_s": round(self.duration, 4),
            "frame_count": self.frame_count,
            "codec": self.stream.codec_context.name or "",
            "pix_fmt": self.stream.codec_context.pix_fmt or "",
        }

    # ---- 帧访问 ---------------------------------------------------------
    def _seek(self, seconds: float):
        """seek 到目标时间附近的关键帧。PyAV 18: 偏移量为 stream.time_base 整数单位。"""
        offset = int(round(seconds / self.time_base))
        self.container.seek(offset, stream=self.stream, backward=True, any_frame=False)

    def _frame_index(self, frame, fallback: int) -> int:
        """帧序号：优先 pts 换算（time_base → 秒 → 帧号），pts 缺失回退计数器。"""
        if frame.pts is not None:
            return int(round(float(frame.pts) * self.time_base * self.frame_rate))
        return fallback

    def frame_at(self, index: int) -> FrameData:
        """按帧序号随机读取（seek 到前一个关键帧后前向解码到目标帧）。"""
        if index < 0:
            raise IndexError(index)
        t = index / self.frame_rate
        self._seek(max(t - 0.3, 0.0))
        decoded = 0
        target = int(index)
        for frame in self.container.decode(self.stream):
            decoded += 1
            if self._frame_index(frame, decoded - 1) >= target:
                idx = self._frame_index(frame, decoded - 1)
                return FrameData(
                    index=idx,
                    time=float(frame.pts) * self.time_base if frame.pts is not None else idx / self.frame_rate,
                    bgr=frame.to_ndarray(format="bgr24"),
                    raw=frame,
                )
            if decoded > 10 * self.frame_rate:  # 安全阀：seek 漂移保护
                break
        # 兜底：从头解码
        self.container.seek(0)
        for frame in self.container.decode(self.stream):
            decoded += 1
            idx = self._frame_index(frame, decoded - 1)
            if idx >= target:
                return FrameData(
                    index=idx,
                    time=float(frame.pts) * self.time_base if frame.pts is not None else idx / self.frame_rate,
                    bgr=frame.to_ndarray(format="bgr24"),
                    raw=frame,
                )
        raise IndexError(f"帧 {index} 超出范围（共 {self.frame_count}）")

    def frames(self, start: int, end: int, step: int = 1):
        """顺序解码 [start, end) 范围内按 step 抽样的帧（高效，适合抓取主循环）。"""
        if step < 1:
            raise ValueError("step 必须 >= 1")
        t = start / self.frame_rate
        self._seek(max(t - 0.2, 0.0))
        seen = -1
        counter = -1
        for frame in self.container.decode(self.stream):
            counter += 1
            idx = self._frame_index(frame, counter)
            if idx < start:
                continue
            if idx >= end:
                break
            if (idx - start) % step == 0:
                seen = idx
                yield FrameData(
                    index=idx,
                    time=float(frame.pts) * self.time_base if frame.pts is not None else idx / self.frame_rate,
                    bgr=frame.to_ndarray(format="bgr24"),
                    raw=frame,
                )
        if seen < 0:
            # seek 漂移：重新顺序扫
            self.container.seek(0)
            counter = -1
            for frame in self.container.decode(self.stream):
                counter += 1
                idx = self._frame_index(frame, counter)
                if idx < start:
                    continue
                if idx >= end:
                    break
                if (idx - start) % step == 0:
                    yield FrameData(
                        index=idx,
                        time=float(frame.pts) * self.time_base if frame.pts is not None else idx / self.frame_rate,
                        bgr=frame.to_ndarray(format="bgr24"),
                        raw=frame,
                    )
