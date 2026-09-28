# esrxp-ng · 硬字幕提取（esrXP 现代化重构）

把 Delphi 5 时代的老牌硬字幕提取工具 esrXP 按逆向原理重写为现代化实现：

- **Rust 后端**：FFmpeg（ffmpeg-next）解码 + 硬字幕抓取引擎 + axum HTTP API
- **GPU 优先**：解码层自动探测 CUDA NVDEC（Windows D3D11VA / Linux VAAPI），失败自动回退 CPU；像素过滤/帧差/缩放内核有 CUDA PTX 与纯 Rust 双实现，运行时按硬件选择
- **TDesign UI**：Vue 3 + TDesign v1.10.5（本地 vendor，无构建链，开箱即用）
- **Electron 外壳**：Win11 目标，electron-builder 出 NSIS 安装包 / 便携版
- **输出**：VobSub（.sub/.idx，4-bit RLE）、SSA（矢量轮廓）、OCR 位图（PNG，按字幕选区裁剪）、SRT 时间轴（无文本）、时间轴 JSON、工程 JSON
- **零运行时依赖**：Debian 版将 FFmpeg 源码编译并静态链入单二进制；Windows 版 FFmpeg 运行时 DLL 随安装包分发，均无需用户另行安装 FFmpeg/Python

## 目录结构

```
esrxp-ng/
├── rust-backend/            # Rust 后端（核心引擎 + API）
│   ├── src/
│   │   ├── config.rs        # 配置模型（对齐 esrXP 设置项语义）
│   │   ├── video.rs         # FFmpeg 解码：硬件优先（CUDA/D3D11VA/VAAPI）+ 软件回退
│   │   ├── filter.rs        # 色彩过滤（HSV 整数公式 + RGB 平方距离 int32）
│   │   ├── postprocess.rs   # 连通域清理（单点/单行/大面积/触边/跨中带）
│   │   ├── ripper.rs        # 抓取引擎：帧差 → 候选 → IoU 分段/去重 → 自动选色
│   │   ├── outputs.rs       # VobSub/SSA/OCR PNG/JSON 产物
│   │   ├── gpu.rs           # 像素内核：CUDA(PTX 动态加载) / CPU 双实现
│   │   ├── api.rs           # axum：open/preview/rip/job/artifact + 静态托管
│   │   └── main.rs          # CLI：rip / serve / dump-config / dbg
│   └── Cargo.toml
├── ui/                      # TDesign Vue3 单页（index.html + vendor/，OCR 选区拖拽）
├── electron/                # Electron 外壳（main/preload/package.json/smoke.js，asarUnpack 解包）
├── tests/                   # 端到端验证产物（rust_out=Rust、out=历史基线）
├── sample_hardsub.mp4       # 测试视频（3 条字幕，真值 0.5–1.5 / 2.0–3.0 / 3.5–5.0）
└── scripts/make_sample_video.py
```

## 快速开始

### 构建 Rust 后端

```bash
# 依赖（Ubuntu/Debian，可换 mirrors.bfsu.edu.cn）
# 动态链接模式（开发验证）：需系统 FFmpeg dev 包
# 静态链接模式（发布，零依赖）：cargo build --release --features vendored-ffmpeg
apt install -y libavformat-dev libavcodec-dev libavutil-dev libswscale-dev \
    libavfilter-dev libswresample-dev libavdevice-dev libclang-14-dev pkg-config

export PATH="$HOME/.cargo/bin:$PATH"
export LIBCLANG_PATH=/usr/lib/llvm-14/lib   # ffmpeg-sys-next bindgen 需要

cd rust-backend
cargo build --release
```

### 命令行抓取

```bash
./target/release/esrxp-ng-server rip sample_hardsub.mp4 --out tests/rust_out
# 产物：sample_hardsub.{ass,sub,idx,timeline.json,esrng.json} + subtitle_imgs/*.png
```

### 图形界面（开发态）

```bash
./target/debug/esrxp-ng-server serve --host 127.0.0.1 --port 18081 --ui ../ui
# 浏览器打开 http://127.0.0.1:18081
```

### Electron 桌面版

```bash
cd electron
npm install
npm start        # 开发运行（自动 spawn 后端二进制）
npm run dist:win # Win11 打包：NSIS 安装包 + 便携版（需 Windows 或 CI）
```

## 逆向原理（从 esrXP）

对原版 esrXP.exe（Delphi 5 PE32）静态逆向得出硬字幕抓取流水线：

1. **解码**：DirectShow 逐帧取图 → 现代化后改为 FFmpeg（硬件解码优先）
2. **帧差检测**：相邻采样帧按 `pixel_difference`（变化像素下限）与 `ignore_change_percent`（占比下限）判断内容变化
3. **色彩过滤**：HSV 与 RGB 三段可组合过滤（`ColorSegment`：hue/hue_diff、rgb/rgb_diff、亮度/饱和度区间），叠加 `pixel_compensate` 像素补偿膨胀
4. **后处理**：连通域级清理（移除单点/单行/大面积块/触边组件，`pass_center` 保留跨中央带的字幕）
5. **字幕状态机**：mask IoU < 0.7 断开内容段；IoU ≥ 0.9 且间隔 ≤ 2 帧合并重复；`gap_frames` 分段；消失帧封口
6. **自动选色**：最亮 15% 中位 = 主色、最暗 15% = 描边，经 `colors_plausible` / `mask_plausible` 门控，不通过则回退配置色
7. **输出**：VobSub 调色板（0=背景、1=主色、2=描边）+ 4-bit RLE 扩展编码；SSA 矢量轮廓

## GPU / CUDA

| 层 | 硬件路径 | 回退 |
| --- | --- | --- |
| 视频解码 | `av_hwdevice_ctx_create` 探测：NVDEC(CUDA) → D3D11VA(Win) / VAAPI(Linux)，`av_hwframe_transfer_data` 取回 | 软件解码（CPU） |
| 像素内核 | `CudaKernels`：libloading 动态加载 libcuda.so.1/nvcuda.dll + 内嵌 PTX（sm_50），整数 HSV 与 CPU 逐位一致 | `CpuKernels` 纯 Rust |

GPU 路径在无 NVIDIA 硬件的环境自动回退 CPU；后端日志输出 `decoder_backend` 与内核 `backend_name()` 供确认。CUDA 内核路径按 Win11 打包口径交付，实机验证需 NVIDIA 环境。

## 验证

- Rust 与 Python 参考实现端到端逐项一致：3/3 字幕，时间轴（0.52–1.52 / 2.00–3.00 / 3.52–4.72）与 bbox 完全一致
- VobSub RLE 编码同款，.sub 结构一致（大小差 <2% 属边缘像素正常差异）
- API 全链路（open/preview/rip/job/artifact）curl 验证通过；Electron 无头冒烟截图验证 UI 渲染与完整交互

## 已知限制

- SSA 轮廓为"连通域+逐行游程端点"剪影式（鲁棒无死循环）；Python 参考实现用 OpenCV 平滑轮廓，视觉细节略优
- `frame_seconds` 在 pts 缺失时以容器帧率兜底（正常视频 pts 均存在）
- CUDA/NVDEC 路径依赖 NVIDIA 硬件，需实机验证
- SRT 纯文本输出未实现（位图字幕用 SRT 表达不了，SSA 已含时间轴）
