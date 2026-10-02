# 更新日志

本项目语义化版本号（SemVer）。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [0.6.2] - 2026-10-02

**全量代码审查修复** —— 逐模块审查（约 6500 行）后修复 1 个功能性 Bug、2 个安全性问题与一批健壮性/工程化问题。

### 修复
- **字幕管理器「移除」后产物不变（P0，功能 Bug）**：软删除（`deleted` 标记）的字幕此前照旧写入 ASS / SRT / VobSub / OCR 位图 / timeline —— 用户以为删掉了，实际产物一条没少（实测：标记数 1，产物仍 3 条）。根因是各写出函数遍历时根本不检查 `deleted`（该字段此前只用于 `.esr` 存取）。现统一在 `write_all_outputs` 入口过滤一次；`.esr` 仍写全量列表（含标记），故标记不丢、可恢复
- **位图旧文件残留**：字幕数量减少后旧 `subtitle_*.png` 留在目录里被误认为本次产物。现写出后按本次编号清理超出范围的旧位图（仅匹配本函数生成的命名前缀与扩展名，不碰用户文件）
- **开发态 CORS 全开（安全）**：`CorsLayer::permissive()` 配合后端接受任意绝对路径，任意网页的 JS 都能借用户权限驱动本机后端（读任意视频、在任意可写位置创建目录并写产物、跑批处理耗 CPU）。现收紧为仅回显本地 Origin。打包态（命名管道 / Unix socket）本不经 CORS，不受影响
- **任务 ID 可预测（安全）**：`rand_id()` 仅用 `subsec_nanos()` 低位与 PID 异或，同进程并发建任务可能撞 `jobs` key，导致 `/api/jobs/{id}` 返回别人的结果。现改为全量纳秒 + PID（rotate）+ 原子序号混合
- **`mask_plausible` 阈值截断**：`count < area * 30 / 100` 的整数除法在小 ROI 上退化为 0（如 area=4 时阈值 1），把本应合理的 mask 误判为不合理。改浮点比较
- **字幕管理器每次操作重开视频**：`write_all_outputs` 为取宽高每次 `VideoSource::open()` 走一遍 FFmpeg 探测，而它被 9 个 manager 端点调用。改为按视频路径进程内缓存宽高，并删除残留死代码（`vinfo`）

### 健壮性
- **`make_event` 全空 mask 兜底**：全空 mask 会让 bbox 算出负宽高并产出尺寸错误的数据。当前所有调用路径均保证非空（不可触发），现补 1x1 空事件防御
- **移除多余 `unsafe` 块**：`frame::Video::empty()` 在当前 ffmpeg-next 版本已是安全 fn

### 工程化
- **CI 新增静态检查 job**（独立于打包流程，`build-win` / `build-debian` 依赖它）：`cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test --lib`。此前 CI 只 build + 打包，`cargo fmt --check` 实测有 diff 也无人拦
- **测试必须显式 `--lib`**：单元测试写在 `src/*.rs` 的 `#[cfg(test)]` 里，裸 `cargo test` 只跑 bin target，显示 `running 0 tests` 静默通过 —— CI 因此固定用 `--lib`
- **补齐单元测试 6 → 17 项**：产物软删除过滤、`.esr` 保留标记、mask 覆盖率门控边界（含 30% 临界）、任务 ID 碰撞、位图序号解析（含 `_top`/`_bottom` 变体与外部文件拒绝）
- **清零编译与 clippy 警告**（原 18 个编译警告 + 22 个 clippy 警告）：删除未使用的 `gpu::backend_name`、并查集 `ptr_arg` 精准豁免（扫描期需 `push` 扩容）、组件号索引循环豁免并说明理由
- **补 LICENSE 文件**：`package.json` 声明 MIT 但仓库无许可声明文本
- **修正文档与实现不符**：`build.yml` 注释写「ffmpeg 4.4」实为 n9.0；Debian job 注释误称需对齐系统 ffmpeg 版本（实为 vendored 源码编译）；README「与 Python 参考实现逐项一致」因 `server.py` 已不在仓库而无法复现，已改为可验证的表述并说明差异

## [0.6.1] - 2026-10-01

**字幕出图除噪与预览对齐修复** —— 按上传的 esrXP 逆向包逐项对照，修复三类出图质量差与预览选区错位（含 `subtitle_0031.png` 所示椒盐噪声样张）。

### 修复
- **OCR / VobSub / 位图缩略图椒盐噪声（根因修复）**：`clean` 只去掉单点与单线（面积 1px），面积 2–5px 的小噪点簇会残留并渲染成雪花噪声。新增连通域级除噪 `despeckle`（8 连通并查集标记 + 按面积阈值剔除），在 OCR 渲染（`render_subtitle_tile`，阈值可配 `output.ocr.despeckle_min_area`，默认 4）与 VobSub 编码（`encode_vobsub_frame`，固定 4）前统一清理，三处输出共用同一除噪逻辑
- **字幕管理器位图缩略图看不清**：manager/list 与各管理器操作此前直接下发 bbox 裁切的原始视频像素（复杂背景 + 字幕混合，缩略图看不清字幕、噪声明显）；现改为下发**渲染后白底黑字位图**（与 OCR 导出同一 `render_subtitle_tile` 管线，含除噪），UI 位图列优先显示渲染图（`render_b64` / `render_w/h`），旧工程与旧后端自动回退原始裁切
- **帧预览叠加错位 / 选区与字幕对不上（根因修复）**：预览整帧（`region_only=false`）时 ROI mask 写回整帧坐标除以缩放因子，而默认配置 `region.scale=0` 时该因子为 0 —— 除法塌缩使全部命中像素落到末行/末列，overlay 高亮画到错误位置、选区框与真实字幕对不上。新增共享 `roi_scale`（`region.scale × preview.scale_video`，≤1e-6 时按不缩放 1.0 处理），`prepare_roi` / `api_preview` / 输出坐标回映统一使用；同时补 `roi_scale` 与 `despeckle` 单元测试防回归

### 验证
- 样例工程渲染位图：白底黑字、背景像素恒为 `#FFFFFF`，除噪后无残留噪点簇
- overlay 预览实测：字幕命中区保持原色（G≈96），背景区染绿（G≈182），字幕区域正确可见且与 bbox 对齐
- `cargo test --lib` 6 项全部通过（新增 despeckle 两项、roi_scale 两项）

### 其他
- 版本 0.6.0 → 0.6.1（Cargo.toml / electron package.json / CHANGELOG 同步）


## [0.6.0] - 2026-09-30

**GPU 内核审查与截图二值参数扩展** —— 逐行审查 CUDA PTX 内核与字幕截图渲染管线，修复取址缺陷，补充可调二值阈值，深色背景拆分支持。

### 修复
- **CUDA 内核取址越界（根因修复）**：`diff_kernel` / `filter_kernel` 用 `mul.wide.u32 p, tid, 3` 直接覆盖 `cvta.to.global` 得到的基址寄存器，再以 `[p + p]` 取址 —— 实际读到 `tid×6` 字节（错位且后半图越界，NVIDIA 实机可能崩溃或产生垃圾数据）。现改用独立偏移寄存器 `off`，基址保持为 `p + off` 形式，与 `scale_kernel`（`[p + si]`）一致
- **两行拆分背景色错误**：`split_vertical` 拆分出的上下两张图用硬编码白色填充，而 `find_blank_split` 已支持深色背景，深色背景下拆分图会出现白条；现拆分图按实际背景色填充
- **空白带检测支持深色背景**：`find_blank_split` 从「整行全白」改为「相对 `bg` 色的容差比较」（容差 12），深色/彩色背景的两行字幕同样能正确拆分

### 新增
- **二值阈值（`binary_threshold`）**：`binary` 渲染模式下覆盖率 ≥ 阈值判为笔画（0.05–0.95，越大笔画越细），替代原先写死的 0.5；UI 在 `color_mode = binary` 时显示对应滑块

### 其他
- `launch` 的 `cuLaunchKernel` 参数数组改为「指向参数值」的指针（此前直接把参数值强转成指针，驱动按宿主地址解引用会崩溃/乱码）
- 版本 0.5.2 → 0.6.0（Cargo.toml / electron package.json / CHANGELOG 同步）


## [0.5.2] - 2026-09-30

**CLI 工程日志顺序修复** —— v0.5.1 实包复验发现：`rip` 子命令的「绑定工程日志」发生在视频信息打印之后，导致视频信息与进度未落入 `<工程目录>/esrxp.log`。

### 修复
- `cmd_rip` 把工程日志绑定提前到打开视频之前；CLI 进度回调同样按 10% 记入工程日志（与 GUI 路径一致）
- 版本 0.5.1 → 0.5.2

## [0.5.1] - 2026-09-30

**CLI 日志自洽** —— 实包端到端复验发现：CLI 路径（`rip` 子命令）只把视频信息与完成汇总打到 stdout，工程日志里缺这两条，日志不自洽。

### 修复
- `esrxp-ng-server rip` 现在把「视频: WxH @ fps, 时长, 帧数」与「抓取完成：处理 N 帧，变化帧 M，字幕 K 条，耗时 T」一并写入 `<工程目录>/esrxp.log`（此前仅 GUI 路径 `/api/rip` 记录）

### 其他
- **实包端到端复验**（v0.5.0 Windows 安装包经 NSIS 两层解包取后端，对真实 1920×1080@60fps 视频前 40s 抓取）：工程日志按预期生成并含任务起止 / 产物写出 / 逐张 OCR 位图 DEBUG 行；产物 ass / srt / sub / idx / esr / subtitle_imgs 齐全；导出画质 —— 用户旧工程 127 张字幕图灰阶数恒为 **2**、过渡像素 **0.00%**，新导出 **28 级灰阶**、**21%–24% 的笔画像素为软边过渡**

## [0.5.0] - 2026-09-29

**截图与预览优化** —— 导出的字幕图黑白二值、放大即糊；预览区显示不全、无缩放手段。

### 背景
- 旧 OCR 影像导出把二值 mask 按**最近邻**直接放大（`sx = (x / scale).floor()`），且只有 `命中→黑 / 其余→白` 两种色值。实测同一样张：scale=1 全图仅 **2 个灰阶**、边缘过渡像素占比 **0.00%**，放大 3 倍后仍是 2 个灰阶 —— 这是「后处理图片质量差」的直接根因
- 预览区把三列拼接图（5760×1080）用 `max-width:100%` 压进卡片，必然整体缩小，且没有任何缩放/平移手段查看细节

### 新增
- **截图设置项**：渲染模式（gray / binary / color）、抗锯齿开关与采样数（1–4）、文字色 / 背景色、缩放比例、缩放滤镜（lanczos3 / catmullrom / triangle / nearest / gaussian）、输出格式（png / jpg / bmp）、图片质量（1–100）、输出最大宽度、每图字幕数、两行拆分
- **图片后处理**：裁剪（上 / 下 / 左 / 右）、四周留边、旋转（0 / 90 / 180 / 270）、水平与垂直镜像、亮度与对比度（-100..100）、转灰度
- **预览查看器**：适应窗口 / 1:1 / 放大 / 缩小 / 全屏灯箱；滚轮以光标为锚点缩放、拖拽平移、双击 1:1、Esc 退出全屏；框选字幕区域在任意缩放平移下仍精确换算为帧像素（含 ROI 偏移与 ROI 缩放）
- **导出效果预览**：`/api/preview` 新增 `ocr` 模式，走与导出**完全一致**的渲染 + 后处理管线，导出前即可看到成品
- **自定义默认参数持久化**：`GET/POST /api/config/file` 落盘 `esrxp-config.json`（位于缓存目录的父目录）；界面提供「保存为默认参数 / 载入自定义默认 / 恢复出厂默认」，启动时自动套用
- **笔画增强**：新增「笔画加粗（`stroke_dilate` 0–4，对覆盖率场做最大值滤波膨胀）」与「覆盖率先验（`coverage_gamma`，默认 0.85，越小笔画越黑越实）」，把细笔画二值 mask 修成清晰字幕
- **运行日志（工程目录同步保存）**：新增 `logging` 模块 —— 内存环形缓冲 + 会话日志 + **工程日志**双通道；日志同步写入 **`<工程目录>/esrxp.log`**（与导出产物同目录）与 `<缓存目录>/esrxp-session.log`；单文件超 4 MiB 自动轮转 `.1/.2/.3`；每行写盘即 flush，崩溃 / 强杀不丢最后线索；时间戳取本地时区（Windows `localtime_s`、POSIX `localtime_r`）
- **日志面板**：UI 新增「日志」tab（刷新 / 每 3 秒自动刷新 / 导出日志到本地 / 显示工程日志与会话日志路径）；前端 `window.onerror` 与 `unhandledrejection` 自动上报为 ERROR 级；打开视频、开始抓取、抓取完成等关键操作一并记录；抓取任务按 10% 记录进度，便于定位中断位置
- **日志接口**：`GET /api/log?path=<工程目录|日志文件>&n=500`（磁盘优先，回退内存环形缓冲；沿用产物目录白名单，且仅允许读取 `esrxp.log` / `esrxp-session.log`，不放宽既有读取面）/ `POST /api/log {project, level, msg}`（供前端上报，并登记该目录以便回读）

### 修复
- **导出字幕图质量**：渲染改为「mask 双线性采样 → N×N 超采样求覆盖率 → 前景 / 背景连续混合」，边缘由硬锯齿变为连续灰度。同一样张实测：灰阶数 **2 → 13**（scale=1）、**2 → 25**（scale=3）；边缘过渡像素占比 **0.00% → 15.62%**
- **预览显示不全**：改为独立视口 + contain 自适应（完整可见、不截断不留白），并可任意缩放查看细节
- 预览整帧路径不再复制整帧 RGB（改为借用切片），降低抓取期内存峰值
- `write_srt_bitmap` 的位图同样走新渲染管线（文件名保持 .bmp 以兼容 SubRip 位图约定）
- OCR 影像文件扩展名随 `output.ocr.format` 变化（png / jpg / bmp），不再固定 `.png`
- **UI 内置默认缺少新增配置项（根因）**：`mergeConfig` 只覆盖界面已知字段，而新增的 `stroke_dilate` / `coverage_gamma` 未加入内置默认配置，导致配置文件中的取值被静默丢弃、控件读不到默认值；已补齐内置默认
- **日志导出参数错误**：`exportLog` 误把路径字符串当作 `saveArtifact({path, name})` 的参数传入（对象形参），改为传对象

### 其他
- `image` crate 启用 `jpeg` feature（JPG 按质量输出；PNG / BMP 为无损，忽略质量项）
- 新增两级验证：离线渲染验证仓（从 `outputs.rs` 抽取渲染管线 + 桩结构独立编译，附画质量化对比、笔画增强对比组、日志双通道落盘回读用例）与前端行为级验证（Playwright + mock 桥：0.5.0 主用例 38 项几何 / 状态断言，补充用例 20 项日志与配置断言）

## [0.4.5] - 2026-09-29

**预览破图修复** —— 点击「预览」后图像区只显示破图与黑色长条（后端已生成 PNG，前端加载不到）。

### 修复
- **预览图加载路径（根因）**：0.4.0 起 UI 由后端 HTTP 托管改为 `loadFile` 以 `file://` 加载，而 `/api/preview` 返回的 `image` 字段仍是相对 URL（`/api/artifact?path=...`），在 `file://` 页面下被解析为 `file:///api/artifact?...`，请求到不了后端，图片必然破图。现 `api_preview` 额外返回 `image_path`（缓存 PNG 绝对路径），前端在 Electron 下统一经 IPC 取二进制转 blob URL 渲染；浏览器直连后端的开发模式仍沿用相对路径
- 预览图 `@load` 后再同步选区框，避免 blob URL 异步加载期间 `getBoundingClientRect` 取到 0 尺寸导致选区框不显示

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
