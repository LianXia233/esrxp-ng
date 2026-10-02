# esrxp-ng · 硬字幕提取（esrXP 现代化重构）

把 Delphi 5 时代的老牌硬字幕提取工具 esrXP 按逆向原理重写为现代化实现：

- **Rust 后端**：FFmpeg（ffmpeg-next）解码 + 硬字幕抓取引擎 + axum HTTP API
- **GPU 优先**：解码层自动探测 CUDA NVDEC（Windows D3D11VA / Linux VAAPI），失败自动回退 CPU；像素过滤/帧差/缩放内核有 CUDA PTX 与纯 Rust 双实现，运行时按硬件选择
- **egui 原生 GUI（MVP，0.7.0 起）**：纯 Rust egui/eframe 桌面界面，覆盖打开视频/工程 → 参数 → 预览 → 抓取 → 产物查看/保存；后端以子进程随 GUI 启动，HTTP 127.0.0.1 通信
- **TDesign UI**：Vue 3 + TDesign v1.10.5（本地 vendor，无构建链，开箱即用）
- **Electron 外壳**：Win11 目标，electron-builder 出 NSIS 安装包 / 便携版；0.4.0 起打包态经命名管道（Windows）/ Unix socket（Linux）与后端通信，**不监听任何网络端口**，UI 本地加载，免疫端口占用与系统代理劫持
- **输出**：VobSub（.sub/.idx，4-bit RLE）、SSA（矢量轮廓）、OCR 位图（PNG/JPG/BMP，按字幕选区裁剪，抗锯齿 + 可后处理）、SRT 时间轴（无文本）、时间轴 JSON、工程 JSON
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
├── egui-ui/                 # egui/eframe 原生 GUI（MVP：打开/参数/预览/抓取/产物）
├── ui/                      # TDesign Vue3 单页（index.html + vendor/，预览查看器 + OCR 选区拖拽）
├── electron/                # Electron 外壳（main/preload/package.json/smoke.js，asarUnpack 解包）
├── tests/                   # 端到端验证产物（rust_out=Rust、out=历史基线）
├── sample_hardsub.mp4       # 测试视频（3 条字幕，真值 0.5–1.5 / 2.0–3.0 / 3.5–5.0）
└── scripts/make_sample_video.py
```

## 快速开始

### 构建 Rust 后端

```bash
# 依赖（Ubuntu/Debian，可换 mirrors.bfsu.edu.cn）
# 动态链接模式（开发验证）：需系统 FFmpeg dev 包（任意 6.x/7.x 均可编译，
#   仅用格式/解码 API；CI 的 lint job 即走此模式）
# 静态链接模式（发布，零依赖）：cargo build --release --features vendored-ffmpeg
apt install -y libavformat-dev libavcodec-dev libavutil-dev libswscale-dev \
    libavfilter-dev libswresample-dev libavdevice-dev libclang-dev pkg-config

export PATH="$HOME/.cargo/bin:$PATH"
export LIBCLANG_PATH=/usr/lib/llvm-14/lib   # ffmpeg-sys-next bindgen 需要

cd rust-backend
cargo build --release
```

> 版本说明：`Cargo.lock` 锁定 `ffmpeg-next 9.0.0`。
> - Windows 发布版：BtbN **n9.0** shared 预编译 DLL 随安装包分发
> - Debian 发布版：`vendored-ffmpeg` 从源码编译 FFmpeg 9.0 并静态链接
> - 开发态：链接系统 FFmpeg，版本不敏感

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

### egui 原生 GUI（0.7.0，MVP）

```bash
# 依赖（Ubuntu/Debian）：egui/eframe 需要 GTK3 与 xkbcommon
apt install -y libgtk-3-dev libxkbcommon-dev

# 先构建后端（egui GUI 以子进程方式启动后端）
cd rust-backend && cargo build && cd ..

cd egui-ui
cargo build --release
./target/release/esrxp-ng-ui   # 自动发现并启动后端（优先 ESRXP_BACKEND / 同目录 / rust-backend 开发产物）
```

MVP 覆盖：打开视频/工程 → 参数配置 → 预览（raw/mask/overlay/combo/ocr）→ 抓取进度 → 产物查看/保存。字幕管理器 / 批处理 / 日志面板留待后续迭代。

### Electron 桌面版

```bash
cd electron
npm install
npm start        # 开发运行（自动 spawn 后端二进制）
npm run dist:win # Win11 打包：NSIS 安装包 + 便携版（需 Windows 或 CI）
```

## 版本与发布

当前版本：**0.7.0**

| 平台 | 产物 | 说明 |
| --- | --- | --- |
| Windows x64 | `.exe`（NSIS 安装包）、`.zip`（便携版） | 后端 `x86_64-pc-windows-gnu` 编译，FFmpeg 9.0 共享库 DLL 随包分发 |
| Debian / Ubuntu x64 | `.deb` | 后端 `--features vendored-ffmpeg` 静态链接，单二进制零依赖 |

下载：仓库 [Releases 页面](https://github.com/LianXia233/esrxp-ng/releases) 提供的最新 tag 资产。

发布由 `.github/workflows/build.yml` 驱动：推送 `v*` tag 触发，静态检查（`cargo fmt --check` + `cargo clippy -D warnings` + `cargo test --lib`）通过后并行产出 Windows 与 Debian 包，`softprops/action-gh-release` 自动创建 Release 并附带 changelog。

> 仓库仅保留最新 Release。此前 v0.1.0 ~ v0.6.0 的历史 Release 与 tag 已于 2026-10-02 清理，
> 版本号可重新使用。v0.6.1 从未产出发布件，其变更已并入 0.6.2 的开发过程，内容完整保留于本文件下方条目。

## 逆向原理（从 esrXP）

对原版 esrXP.exe（Delphi 5 PE32）静态逆向得出硬字幕抓取流水线：

1. **解码**：DirectShow 逐帧取图 → 现代化后改为 FFmpeg（硬件解码优先）
2. **帧差检测**：相邻采样帧按 `pixel_difference`（变化像素下限）与 `ignore_change_percent`（占比下限）判断内容变化
3. **色彩过滤**：HSV 与 RGB 三段可组合过滤（`ColorSegment`：hue/hue_diff、rgb/rgb_diff、亮度/饱和度区间），叠加 `pixel_compensate` 像素补偿膨胀
4. **后处理**：连通域级清理（移除单点/单行/大面积块/触边组件，`pass_center` 保留跨中央带的字幕）
5. **字幕状态机**：mask IoU < 0.7 断开内容段；IoU ≥ 0.9 且间隔 ≤ 2 帧合并重复；`gap_frames` 分段；消失帧封口
6. **自动选色**：最亮 15% 中位 = 主色、最暗 15% = 描边，经 `colors_plausible` / `mask_plausible` 门控，不通过则回退配置色
7. **输出**：VobSub 调色板（0=背景、1=主色、2=描边）+ 4-bit RLE 扩展编码；SSA 矢量轮廓

## 截图与后处理（0.5.0）

导出的字幕图不再是二值 mask 的最近邻放大，而是**按覆盖率重建的抗锯齿图像**：

1. `mask` 双线性采样把二值场变成连续场；每个输出像素再取 N×N 子采样求平均 → 得到 0..1 的边缘覆盖率
2. 按覆盖率在前景色与背景色之间线性混合：`gray`（默认，白底黑字、边缘平滑）、`color`（保留原视频像素色）、`binary`（旧版二值硬边，兼容用）
3. 需要更实的效果时叠加**笔画增强**：`stroke_dilate`（0–4）对覆盖率场做形态学膨胀使细笔画变粗，`coverage_gamma`（默认 0.85）做覆盖率先验让笔画更黑；两者都作用于软化后的覆盖率场，不会退回硬锯齿
4. 之后依次应用后处理：裁剪 → 留边 → 旋转 / 镜像 → 亮度 / 对比度 / 灰度 → 最大宽度限制（lanczos3 等滤镜）

| 指标（同一样张） | 旧 binary + 最近邻 | 新 gray + 抗锯齿 |
| --- | --- | --- |
| 灰阶数（scale=1 / 3） | 2 / 2 | 13 / 25 |
| 边缘过渡像素占比（scale=1） | 0.00% | 15.62% |

笔画增强实测（scale=3，合成样张；笔画占比 = 暗像素占全图比例，越高越「实」）：

| 参数 | 灰阶数 | 中间灰阶占比 | 笔画占比 |
| --- | --- | --- | --- |
| gamma=1.00 dilate=0 | 25 | 10.92% | 63.07% |
| gamma=0.85 dilate=0（默认） | 26 | 9.61% | 63.33% |
| gamma=0.85 dilate=1 | 26 | 9.70% | 66.71% |
| gamma=0.70 dilate=2 | 26 | 9.91% | 70.12% |

加粗走的是「软边加粗」：灰阶数不降（保持 26 级），说明没有退回二值硬边。

**除噪（0.6.1）**：渲染前先做连通域级除噪（`despeckle`，8 连通并查集按面积阈值剔除孤立小分量），消除抓取残留在 mask 上的椒盐噪声点簇——OCR 截图、VobSub 字幕图、字幕管理器位图缩略图三处输出共用同一除噪逻辑（阈值 `output.ocr.despeckle_min_area`，默认 4，设为 0 关闭）。字幕管理器位图列现直接显示**渲染后的白底黑字图**（与 OCR 导出同一管线），不再展示原始视频像素，缩略图清晰可读。

预览区为独立视口（contain 自适应，图像完整可见），支持滚轮缩放（以光标为锚点）、拖拽平移、双击 1:1、全屏灯箱；「导出效果」模式直接渲染导出成品。参数经「保存为默认参数」落盘为 `esrxp-config.json`，启动时自动套用。帧预览叠加（overlay）与框选选区严格对齐真实字幕（共享 `roi_scale` 统一 ROI 缩放换算，默认配置下不再除零错位）。

## 字幕管理器（.esr 工程）

`.esr` 工程文件保存配置、字幕（含位图与删除标记）与被过滤候选，可反复打开编辑：

| 操作 | 语义 |
| --- | --- |
| 移除（Remove） | **软删除**：打 `deleted` 标记，立即从所有产物（SSA / VobSub / SRT / OCR 位图 / timeline）中排除；标记与数据仍留在 `.esr`，可 Recover Filtered 或重新打开工程后恢复 |
| 彻底清除（Purge） | 物理丢弃所有已标记的字幕，不可恢复 |
| 恢复（Recover Filtered） | 把被过滤掉的候选重新并入事件列表 |

写产物时统一在 `write_all_outputs` 入口过滤软删除项，各写出函数无需各自判断；位图目录
会同步清理超出本次编号范围的旧 `subtitle_*.png`，避免残留被误认为本次产物。

## 运行日志（0.5.0）

日志同时写两条通道，均逐行写入并 flush（进程崩溃、强杀都留得下最后一条线索）：

| 通道 | 路径 | 用途 |
| --- | --- | --- |
| 工程日志 | `<工程目录>/esrxp.log` | 与导出产物同目录保存，随工程一起交付 / 复现问题 |
| 会话日志 | `<缓存目录>/esrxp-session.log` | 尚未选定工程时的兜底，跨工程留痕 |

- 记录内容：后端启动信息、打开视频 / 工程、抓取任务起止与每 10% 进度、产物写出明细、OCR 位图逐张写出（DEBUG）、字幕管理器操作、配置保存、全部 ERROR / WARN；前端 `window.onerror`、`unhandledrejection` 与打开视频 / 开始抓取等关键操作
- 轮转：单文件超过 4 MiB 时 `esrxp.log → esrxp.log.1 → .2 → .3`，最多保留 3 个备份
- 时间戳：本地时区（Windows 用 `localtime_s`，POSIX 用 `localtime_r`）
- 界面：「日志」tab —— 刷新 / 每 3 秒自动刷新 / 导出日志到本地，并显示两条日志的实际路径
- 接口：`GET /api/log?path=<工程目录>&n=500`（读磁盘，回退内存环形缓冲）、`POST /api/log {project, level, msg}`（前端上报）

## GPU / CUDA

| 层 | 硬件路径 | 回退 |
| --- | --- | --- |
| 视频解码 | `av_hwdevice_ctx_create` 探测：NVDEC(CUDA) → D3D11VA(Win) / VAAPI(Linux)，`av_hwframe_transfer_data` 取回 | 软件解码（CPU，默认） |
| 像素内核 | `CudaKernels`：libloading 动态加载 libcuda.so.1/nvcuda.dll + 内嵌 PTX（sm_50），整数 HSV 与 CPU 逐位一致 | `CpuKernels` 纯 Rust |

GPU 路径在无 NVIDIA 硬件的环境自动回退 CPU；后端日志输出 `decoder_backend` 供确认。CUDA 内核路径按 Win11 打包口径交付，实机验证需 NVIDIA 环境。

> 视频解码默认走 CPU 软解：0.4.2 起 Windows GNU 构建 + 共享 DLL 组合下 NVDEC/D3D11VA
> 实测存在堆损坏（0xC0000374）。本工具为离线处理场景，CPU 足够。如需强制启用硬件
> 探测，设置环境变量 `ESRXP_HWDEC=1`（仅供排查 hw 路径问题）。

## 验证

- 端到端样张：`sample_hardsub.mp4`（3 条字幕，真值 0.5–1.5 / 2.0–3.0 / 3.5–5.0）抓取结果与真值一致
- 单元测试：`cargo test --lib` 覆盖 ROI 缩放换算、连通域除噪、mask 覆盖率门控边界、任务 ID 碰撞、产物软删除过滤、位图序号解析
- 静态检查：CI 强制 `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings`
- API 全链路（open/preview/rip/job/artifact）curl 验证通过；Electron 无头冒烟截图验证 UI 渲染与完整交互

> 注：早期版本的「Python 参考实现逐项一致」结论已无法复现——`server.py` 参考实现已不在本仓库，
> 仅保留 `scripts/make_sample_video.py`（样张生成）。如需重新做交叉验证，需先补回参考实现。

## 已知限制

- SSA 轮廓为"连通域+逐行游程端点"剪影式（鲁棒无死循环）；Python 参考实现用 OpenCV 平滑轮廓，视觉细节略优
- `frame_seconds` 在 pts 缺失时以容器帧率兜底（正常视频 pts 均存在）
- CUDA/NVDEC 路径依赖 NVIDIA 硬件，需实机验证
- SRT 纯文本输出未实现（位图字幕用 SRT 表达不了，SSA 已含时间轴）
