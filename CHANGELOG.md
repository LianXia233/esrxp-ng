# 更新日志

本项目语义化版本号（SemVer）。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [0.1.1] - 2026-09-29

### 新增
- **SRT 时间轴导出**：仅保留字幕图片所在时间轴（无文本），作为位图字幕的时间索引，配合 OCR 位图/SSA 使用
- **OCR 字幕截图手动选区**：预览图直接拖拽框选字幕区域（不必全屏截图），OCR 位图按所选区域裁剪；区域参数与选区双向联动
- **强制合并相邻字幕**（字幕管理器 Force Merge）：同区相邻字幕强制并为一条
- **更高质量轮廓**（Better Quality）：SSA 矢量轮廓 RDP 阈值收紧，保留更多字形细节
- **附加颜色**（Additional Color）：支持多组附加颜色段联合过滤
- **启用过滤开关**（Enable Filter）、SSA 字体选择、SSA 非默认样式名
- **Debian 静态链接**：FFmpeg 源码编译并静态链入单二进制，用户安装零依赖（无需安装 FFmpeg）

### 修复
- Windows 编译链路：改用 mingw-w64 GNU + BtbN FFmpeg 4.4（MSVC cl -I 解析问题、FFmpeg 9.0.2 头不匹配）
- Debian 编译链路：改用 ubuntu-22.04 + electron-builder homepage 配置
- Electron 打包：server/ui 目录 asarUnpack 解包，Rust 后端与 FFmpeg 运行时从真实路径加载

### 其他
- 移除 Python 参考实现（esrxp_ng/），功能全部由 Rust 实现
## [0.1.0] - 2026-09-29

**初始发布** —— esrXP（Delphi 5 硬字幕提取工具）现代化重构。

### 新增

- **Rust 后端**（`rust-backend/`）
  - FFmpeg（ffmpeg-next）解码：CUDA NVDEC → D3D11VA（Windows）/ VAAPI（Linux）硬件优先，失败自动回退软件解码
  - 硬字幕抓取引擎：帧差检测 → 色彩过滤（HSV 整数公式 + RGB 平方距离 int32）→ 连通域后处理 → IoU 分段/去重 → 自动选色
  - axum HTTP API：`/api/info`、`/api/video/open`、`/api/preview`、`/api/rip`、`/api/jobs/{id}`、`/api/artifact` + 静态托管
  - GPU/CUDA 像素内核（过滤/帧差/缩放）：CUDA PTX 动态加载与纯 Rust CPU 双实现，按硬件自动选择
- **TDesign Vue3 桌面 UI**（`ui/`）：打开视频 → 参数配置（抓取/区域/过滤/后处理/输出）→ 三栏帧预览 → 抓取进度 → 产物下载；本地 vendor 无构建链
- **Electron 外壳**（`electron/`）：自动拉起后端、原生文件对话框桥接、electron-builder Win11 打包配置
- **输出格式**：VobSub（.sub/.idx，4-bit RLE 扩展编码）、SSA（矢量轮廓）、OCR 位图（PNG）、时间轴 JSON、工程 JSON
- **持续集成**：`.github/workflows/build.yml` 自动编译 Windows / Debian 客户端并上传到 GitHub Releases
- Python 参考实现（PyAV + OpenCV）作为验证基准

### 验证

- Rust 与 Python 参考实现端到端逐项一致：3/3 字幕，时间轴（0.52–1.52 / 2.00–3.00 / 3.52–4.72）与 bbox 完全一致
- VobSub RLE 编码同款，.sub 结构一致
- Electron 无头冒烟：UI 渲染、视频打开、三栏预览、抓取全链路通过

### 已知限制

- SSA 轮廓为连通域+逐行游程端点的剪影式（鲁棒无死循环）；Python 参考实现为 OpenCV 平滑轮廓，视觉细节略优
- CUDA / NVDEC 路径依赖 NVIDIA 硬件，需实机验证
- SRT 纯文本输出暂未实现（位图字幕用 SRT 表达不了，SSA 已含时间轴）
