# 更新日志

本项目语义化版本号（SemVer）。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [0.4.4] - 2026-09-29

**全量代码审计修复** —— 逐行审计全部源码（rust-backend / electron / ui / CI）后集中修复。

### 安全
- **`/api/artifact` 任意路径读取修复**：新增产物目录白名单（preview 缓存目录、rip/batch 输出目录、工程目录；canonicalize 规避 `..` 与符号链接绕过），白名单外路径一律 403

### 修复
- **合并重复后位图与 mask 尺寸不一致**：`merge_repeat_with` 中 mask 按并集 bbox 重算，但 image 直接沿用被合并事件的 bbox 尺寸，两事件 bbox 不同时违反同尺寸不变式（下游 VobSub/SSA 渲染存在越界/错位风险）；现按并集 bbox 从 ROI 画布重建 image
- **CLI flag 缺值崩溃**：`parse_flag` 在 flag 位于参数末尾（无值）时索引越界 panic，改用 `args.get` 安全访问
- **事件时长取整失效**：`duration` 表达式的 `.round()` 误作用于字面量 `100.0f64`（恒为 100），修正括号使两位小数四舍五入生效
- **任务 started 恒为 0**：`Job.started` 误用 `Instant::now().elapsed()`（对刚创建的计时器恒 0），改为记录创建时刻的 Unix 时间戳（秒）
- **预览缓存文件名碰撞**：`preview_{微秒%1e6}.png` 取模可能碰撞覆盖，改自增序号并滚动清理（保留最近 32 张）；同时删除 `api_preview` 中被完全覆盖的死代码拼接循环

### 其他
- CUDA 计算后端 PTX 修复（删除 `filter_kernel` 中未声明寄存器 `tid2`/`rslt` 死代码；此前 `cuModuleLoadDataEx` 必然编译失败，CUDA 内核自 0.4.0 起静默回退 CPU）随本版生效

## [0.4.3] - 2026-09-29

**预览修复** —— 打开视频后预览第 0 帧报「帧不存在」。

### 修复
- **首帧锚定**：录屏等来源的视频首帧 pts 常带偏移（如 0.02s），`frame_index` 按 pts 换算帧序号会把请求区间 `[start, end)` 的起点跳过（首帧换算 idx > start 且此前无任何产出），导致 `start=0`（UI 默认预览帧）时解码区间为空、报「帧不存在」。现于软件/硬件两条解码路径加入首帧锚定：本段解码的第一帧若换算 idx 越过区间起点，则锚定到起点

## [0.4.2] - 2026-09-29

**后端崩溃修复** —— 预览/抓取过程中后端异常终止（退出码 3221226356 / 0xC0000374）。

### 修复
- **hw 解码堆损坏（根因修复）**：`try_decode_hw` 将 `hw_frames_ctx` 所有权移交解码器后，清理路径 `avcodec_free_context` 已释放该 buffer，代码仍对原指针 `av_buffer_unref`，构成 use-after-free，Windows 构建下 preview/rip 必触发堆崩溃；现移交后置空本地指针，统一由 `avcodec_free_context` 释放
- **Windows 默认回退纯 CPU 解码**：hw 解码链（NVDEC/D3D11VA）在 Windows GNU 构建 + FFmpeg 共享 DLL 组合下存在堆损坏（0.3.x-0.4.1 各版本实测必崩，Linux CPU 路径从不复现），本工具为离线处理场景，默认禁用 hw 解码；设环境变量 `ESRXP_HWDEC=1` 可强制启用 hw 探测，仅用于排查

### 排查备注
- 打开特定 MP4 时出现的 `UDTA parsing failed retrying raw` 为 FFmpeg 对非标 metadata atom 的降级警告（ffmpeg 8.1.2 独立构建同样输出），与本崩溃无因果关系；如需消除可 `ffmpeg -i in.mp4 -map 0 -c copy out.mp4` 转存

## [0.4.1] - 2026-09-29

**esrXP 对齐补全** —— Merge Repeat 一键合并 + 管理器位图缩放预览。

### 修复
- 修正 `merge_repeat_manual` 返回表达式引发的 E0382 编译失败（元组按序求值中值先被移动后被借用）
- 修复预览等 IPC 请求报 `An object could not be cloned`：Vue 响应式对象不可结构化克隆，`api()` 入口统一深拷贝为纯 JSON 后再经桥转发

### 新增
- **合并重复（MIMergeRepeat 对齐）**：`POST /api/manager/merge_repeat`，对工程内全部未删除字幕按 mask IoU 相似度一键合并（可调 `iou_threshold` 默认 0.9、`max_gap_s` 默认 0.2 秒），操作后重导出全部产物；管理器工具行新增「合并重复」按钮
- **管理器位图列与缩放预览（Manager Zoom 对齐）**：manager/list 与各管理器操作响应新增字幕位图（bbox 裁切 RGB24 base64），表格新增位图列（canvas 渲染、事件级缓存），工具行新增 50%-400% 缩放控件（步进 50%）
- 默认值校准实测完成（GitHub 云编译 Linux 产物 + 样片 A/B）：当前默认 pixel_difference=20 / ignore_change_percent=0.5 / frame_skip=1 三条字幕全部命中真值（IoU=1.0）；esrXP 出厂值 80/80 在本实现语义下完全失效（0 条检出），确认默认值维持不变

## [0.4.0] - 2026-09-29

**去端口化架构** —— Electron 与后端改走进程内管道，彻底消除端口/代理/跨进程失联类故障。

### 变更
- **后端新增 `serve --pipe` 模式**：Windows 命名管道（`\\.\pipe\esrxp-ng-backend`）/ Unix domain socket 监听，hyper 直接服务 HTTP/1.1，协议语义与 TCP 模式完全一致；`--host/--port` TCP 模式保留（开发/浏览器调试用）
- **Electron 不再开任何网络端口**：UI 改为 `loadFile` 直接从磁盘加载；渲染进程全部 API 请求经 preload contextBridge → 主进程 → `http.request({socketPath})` 管道转发后端；产物下载改为「另存为」对话框直存本地。端口占用误判、系统代理劫持 loopback（Failed to fetch）两类问题连根消除
- **单实例锁**（`requestSingleInstanceLock`）：0.3.2 实测双实例导致第二实例后端起不来、错挂到第一实例后端，后端一崩 UI 全线断连；现二次启动自动聚焦既有窗口
- **后端运行期崩溃告警**：后端进程异常退出时弹出明确错误框（含退出码与 stderr 尾部），不再静默 Failed to fetch；退出码与 stderr 尾部同步写入 startup.log
- 打包态与开发态（`npm start`）均走管道；浏览器直连后端的开发方式不变（`serve --port`）

### 升级说明
- 版本号跨次版本号（0.3 → 0.4）：对外 HTTP API 无变化，仅传输层从 TCP localhost 换为管道；命令行 `rip` / `dump-config` / `dbg` 用法不变

## [0.3.2] - 2026-09-29

**修复 Windows 启动白屏根因** —— UI 资产未进包，恢复 GPU 硬件加速。

### 修复
- **UI 资产未打包（白屏根因）**：`build.files` 中的 `../ui/**/*` 被 electron-builder 静默忽略（files glob 不允许跳出应用目录），打包含 0.3.0/0.3.1 的客户端后端静态托管目录为空，`GET /` 返回 404 空响应体，Electron 渲染空白页即白屏。改为 `extraResources` 复制 ui 到 `resources/ui`，`resolveUiDir` 优先读取并写入启动日志
- **恢复 GPU 硬件加速**：撤销 0.3.1 的全局 `app.disableHardwareAcceleration()`；个别环境渲染异常可用 `--disable-gpu` 启动参数兜底
- 清理 main.js 中 0.3.1 遗留的死代码（`unpackedBase`）与过时注释
- 修正 CHANGELOG 中重复的 0.3.0 标题

### 说明
- 0.3.1 的防御措施全部保留：端口占用自动规避（18081-18085）、`/api/info` 版本校验、`userData/startup.log` 启动日志、渲染进程崩溃与加载失败兜底
- 验证方法：安装后 `curl --noproxy "*" http://127.0.0.1:18080/api/info` 应返回版本 JSON；`GET /` 应返回 index.html

## [0.3.1] - 2026-09-29

**修复 Windows 启动白屏** —— 全面防御 + 可诊断。

### 修复
- **禁用 GPU 硬件加速**：Win11 虚拟机 / RDP / 无显卡驱动环境 Electron 渲染白屏，改软件渲染稳定显示
- **端口占用自动规避**：18080 被占用时自动改用 18081-18085 重试；/api/info 校验 JSON version 字段，避免被其他 HTTP 服务误判为后端（此前会加载错误页面导致白屏）
- **启动诊断**：后端 stderr / 选端口 / 加载 URL 全部写入 用户数据目录/startup.log
- **加载兜底**：渲染进程崩溃或页面加载失败时弹出明确错误框 + 错误页，不再无声白屏
- **窗口优化**：ready-to-show 后再显示窗口，消除启动白屏闪烁

## [0.3.0] - 2026-09-29

**补齐最后一处未对齐** —— 字幕管理器时间轴编辑（esrXP Subtitle Manager 手动编辑时间轴）。

### 新增
- **时间轴编辑**：`/api/manager/edit` 单条直接改 start/end（秒），帧号按视频 fps 自动换算并持久化到 .esr
- **时间平移**：`/api/manager/shift` offset_ms 平移（全部保留字幕或选中 indexes），对齐 esrXP Time Shift
- **分割**：`/api/manager/split` 在指定秒处把一条字幕切成两条，各自按新区间解码重抓（bbox/mask 精确）
- **合并**：`/api/manager/merge` 两条字幕合并为一条，按合并区间解码重抓
- **UI 字幕管理器**：表格 start/end 直接可编辑（t-input-number）、平移/分割/合并控件行

### 修复
- 时间轴编辑后 .esr 工程与全部产物（SSA/VobSub/SRT/SRT+bitmap/OCR/.esr）同步重导出

## [0.2.0] - 2026-09-29
## [0.2.0] - 2026-09-29

**功能全部对齐 esrXP beta 10** —— 按逆向分析报告（`esrXP_re/esrXP逆向分析报告.md`）第 6 节功能对照表逐项补齐。

### 新增
- **批处理（Batch）**：`/api/batch` 多视频批量抓取，UI 新增批处理卡片（多文件输入 + 状态/进度表）
- **字幕管理器（Subtitle Manager）**：`/api/manager` list / recover / remove（标记删除）/ purge / crop / export；UI 视图（全部/隐藏已删/仅已删）、全选、恢复被过滤候选、标记删除选中、清除已删、裁剪选中、重新导出
- **OCR 截图参数**：per_image（每图字幕数，多字幕拼一张）、scale（放大）、divid_into_2_lines（空白带检测拆两行）
- **SRT 带位图（SubRip with bitmap）**：`output.srt_bitmap`，SRT 仅保留字幕图片所在时间轴（不含文本）+ 每字幕独立 `.bmp`（白底黑字）
- **样式全字段对齐**（SSA）：Unicode 开关、Time Shift（±10ms 平移）、Outline Width、Shadow Depth、Primary/Secondary/Outline/Shadow 四色
- **预览控制**：Scale Video（整帧缩放）、Sharpen Video、White Background、Slow Speed，均入 `preview` 配置段并作用于 ROI/OCR 截图
- **Pixel Color 取色**：`/api/pixel` + UI 预览图第一栏点击取主色/描边色（Alt 点击取描边色）
- **打开工程 .esr**：`/api/project/open`，UI 可直接打开 v2 工程（JSON，含 image/mask/roi_mask base64、deleted/filtered 状态）
- **.esr 工程 v2**：升级为自包含工程（内嵌字幕位图/掩码/ROI 掩码/删除与过滤状态），管理器操作后重新导出全部产物
- **CLI 对齐**：`rip` 子命令改为与 API 同一套 `write_all_outputs` 产物管线（SSA/VobSub/OCR/SRT/SRT+bitmap/JSON/.esr）

### 修复
- image crate 增加 `bmp` feature（修复 "The image format `Bmp` is not supported"）
- SRT+bitmap 输出目录层级（`<stem>_subs/` 与 SRT 同级）
- 批处理闭包借用、manager 路由 `?` 编译错误（改用 match 显式返回）
- 字幕管理器表格 duration 字段由前端按 start/end 计算

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
