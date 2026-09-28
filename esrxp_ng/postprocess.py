"""后处理 —— 对应 esrXP 的 TFPostprocessingSetting，用 OpenCV 连通域向量化实现。

- single_dot   : 移除只有一像素的点（连通域面积 == 1）
- single_line  : 移除只有一像素的线（宽度或高度为 1 且面积很小的细条）
- large_block  : 移除区块大过 N 像素的连通域（避免把画面大块误判为字幕）
- touch_edge   : 移除接触区域边缘的连通域
- pass_center  : 移除通过中央的区块（跨越区域中心水平带的连通域，常用于去除
                 画面中央的动态对象而保留底部字幕）
"""
from __future__ import annotations

import cv2
import numpy as np

from .config import PostprocessConfig


def clean(mask: np.ndarray, pcfg: PostprocessConfig) -> np.ndarray:
    """对二值 mask 执行连通域级清理，返回 0/255 mask。"""
    if mask.dtype != np.uint8:
        mask = (mask > 0).astype(np.uint8) * 255
    num, labels, stats, _ = cv2.connectedComponentsWithStats(mask, connectivity=8)
    if num <= 1:
        return mask.copy()

    keep = np.zeros(mask.shape, dtype=np.uint8)
    h, w = mask.shape
    cx = w // 2
    center_band_y0 = int(h * (0.5 - pcfg.center_tolerance))
    center_band_y1 = int(h * (0.5 + pcfg.center_tolerance))

    for lbl in range(1, num):
        x, y, bw, bh, area = stats[lbl]
        if area <= 0:
            continue
        # 单像素点 / 单像素线
        if pcfg.single_dot and area == 1:
            continue
        if pcfg.single_line and area <= 4 and (bw == 1 or bh == 1):
            continue
        # 大块
        if pcfg.large_block > 0 and area > pcfg.large_block:
            continue
        # 接触边缘
        if pcfg.touch_edge and (x <= 0 or y <= 0 or x + bw >= w or y + bh >= h):
            continue
        # 通过中央
        if pcfg.pass_center and y < center_band_y1 and y + bh > center_band_y0:
            # 仅当该区块在中央带内同时上下都未完全包含时才移除（保留真正贴底的整条字幕）
            if y < center_band_y0 and y + bh > center_band_y1:
                continue
        keep[labels == lbl] = 255
    return keep
