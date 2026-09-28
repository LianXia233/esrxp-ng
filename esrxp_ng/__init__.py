"""esrxp-ng —— 硬字幕提取工具的现代化重构。

基于对 esrXP beta 10 (2005) 的逆向分析，用现代技术栈重写其核心原理：
- 解码：PyAV（绑定 FFmpeg 库），替代 2005 年的 DirectShow 管线
- 像素处理：numpy 向量化 + OpenCV 形态学/连通域，替代 GR32 逐像素 Delphi 代码
- 算法模型：忠实还原 esrXP 的 抓取(RipOption) → 三段色彩过滤(Outline/Pass1/Final)
  → 后处理(单点/单线/大块/触边/过中心) → 字幕分段 → 输出(.esr/.srt/.ssa/.idx+.sub/OCR)
"""

__version__ = "0.1.0"
