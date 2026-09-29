# esrXP.exe 深度逆向分析报告

> 分析对象：`esrXP.exe`（2,123,776 B，MD5 `1d2cea970f70f6c90b070128df68685b`）
> 工具链：pefile 2024.8.26 + capstone 5.0.7 + strings/objdump（Linux 沙箱静态分析，未运行样本）
> 版本串：`esrXP beta 10 (20051214)` —— 2005-12-14 构建的 beta 10
> 日期：2026-09-29

---

## 1. 二进制与编译链（PE 取证）

| 项 | 值 | 取证来源 |
|---|---|---|
| CPU 架构 | x86 PE32（Machine=0x14C） | PE 头 |
| 链接器 | Borland Linker **5.0**（Delphi 5/6/7 系） | OptionalHeader.LinkerVer |
| ImageBase | 0x400000 | PE 头 |
| 入口点 | 0x1684 | PE 头 |
| 节区 | 8 个：`.text`(1.48MB) `.data`(88KB) `.tls` `.rdata` `.idata` `.edata`(224KB) `.rsrc`(168KB) `.reloc` | 节区表 |
| 子系统 | Windows GUI（Subsystem=2） | PE 头 |
| TimeDateStamp | 0x43A02E64（2005-12-13 UTC） | PE 头 |
| Checksum | 0x0（非官方正式构建，符合 beta 版特征） | PE 头 |

**第三方组件（RTTI/字符串取证）**：
- **Graphics32（Gr32）**：`TPalette32`、`TStretchFilter`、`TPerformanceGraph` —— 位图操作、缩放（TStretchFilter）、调色板、性能图全部基于 Graphics32
- **Tnt Unicode 控件集**：`TTntFrame`、`TTntPicture`、`TTntCustomFrame`、`TTntFileSaveAs` —— 界面使用 Tnt 控件，**天然支持 Unicode 文件名/路径**
- **VirtualTreeView**：`TVirtualTree*` 系列 —— 字幕列表用虚拟树（大批量条目 UI）
- 系统控件：COMCTL32 24 函数（公共控件）、GDI32 107 函数（绘图）

**解码管线取证**：导入表 **无** quartz.dll/strmbase（DirectShow 无静态导入）→ DirectShow 解码在运行时通过 COM 动态创建：
- `CoCreateInstance`（OLE32）+ Unicode 串 `\SOFTWARE\Classes\CLSID\%s`（查 CLSID 注册表）
- 结合 `TPerformanceGraph` 与早期架构分析 → **DirectShow FilterGraph 运行时组装**（文件源 → 分离器 → 解码器 → 采样器）

---

## 2. 注册表 / 配置 Schema（字符串取证）

- 键路径：**`esrXP\setting\`**（.data 0x57920C，实际引用 HKCU 根下的 `software\esrXP\setting\` 结构）
- 值名（.data 0x5795D0 区域）：**`RipSubtitleColor`**（字幕主色）、**`RipOutlineColor`**（描边色）—— 与 `ASaveSetting`/`ADeleteSetting` 菜单动作（保存/删除设置）配套
- 另有 `Language` 串（多语言 ini 机制，配 English.ini / Simplified Chinese.ini / Tranditional Chinese.ini）

---

## 3. 功能字典（全量，与 English.ini 逐项对照）

### 3.1 主窗体（[FMain]）
File（Open/Open Video/Save As/Save OCR Image/Batch/Exit）、Subtitle（Filter/Rip Option/Manger/Subtitle Style）、Option（Merge Repeat/Scale Video/Sharpen Video/White Background/Additional Color/Select Font/Slow Speed）、Help（About）、Start/Stop。

### 3.2 滤镜设置（[FFilter]）
- **Zoom / Region**（GBZoom）：Full Width 复选框、Region 上下左右 —— 抓取区域（即"手动选择字幕所在区域"，不整屏截图）
- **Filter Setting**：Subtitle Color（主色）、Outline Color（描边色）、**Filter Method = Color / Color + Outline**、**Enable Filter** 开关、**Additional Color**（附加颜色）、Advance（进入高级三段设置）、Postprocessing（后处理）、Preview（预览）、Pixel Color（像素取色）

### 3.3 高级滤镜（[FNewOutlineSetting] —— 三段色彩过滤核心）
```
┌─ Final（最终过滤）
│    Hue Difference / RGB Difference / Lum Min / Lum Max / Sat Min / Sat Max
├─ Pass1（第一遍：轮廓过滤）
│    Hue Difference / RGB Difference / Lum Min/Max / Sat Min/Max
├─ Outline（描边色）
│    Hue Difference / RGB Difference / Lum Min/Max / Sat Min/Max
└─ Pixel Compensate（像素补偿）
```
**阈值控件取证（DFM）**：全部 TEdit+TUpDown，**范围 0–99**（LumMin Min=0 Max=0x63、SatMin Min=0 Max=0x63 等）。`Recommand Setting`（推荐设置）一键填默认阈值。

### 3.4 后处理（[FPostprocessingSetting] —— 5 种清理）
Remove single pixel dot（单点）/ single pixel line（单线）/ block larger than N pixels（大面积块，阈值可设）/ block touch edge（触边）/ block pass center（跨中带）。

### 3.5 抓取选项（[FRipOption]）
- **Frame Skip**（跳帧采样：每 N 帧处理 1 帧）
- **Pixel Difference**（像素差异判定阈值）
- **Ignore Change (%)**（忽略变化百分比 —— 去闪烁/微动噪点）

### 3.6 字幕管理器（[FManger]）
All Subtitle / Hide Deleted / Show Deleted（显示过滤）、Additional Color、**Force Merge**（强制合并相邻字幕）、Recover Filtered（恢复被过滤）、Remove Passed（移除已抓）、Crop Subtitle（裁剪字幕）、Purge Deleted（清除已删）、**Better Quality**（更高质量轮廓）。

### 3.7 OCR 图片（[FOCRImage]）
Subtitle Per Image（每图字幕数）、Scale Subtitle（放大倍数）、Divid each subtitle into 2 lines（每字幕拆 2 行）。

### 3.8 字幕样式（[FSubtitleStyle]）
- **No Default Subtitle Style**（非默认样式）
- **Use Unicode Output**（Unicode 输出）
- **Time Shift (10ms)**（时间平移，单位 10 毫秒）
- Primary / Secondary / Outline / Shadow 四色
- Outline Width（描边宽度）、Shadow Depth（阴影深度）

### 3.9 批量（[FBatch]）
Add Files / Remove / Start —— 批量多文件处理。

---

## 4. 输出格式逆向（保存对话框 Filter 全量）

保存过滤器字符串（.rsrc DFM，0x5E9D98）：
```
esrXP (*.esr)|*.esr
Sub Station Alpha (*.ssa)|*.ssa
SubRip (*.srt)|*.srt
SubRip with bitmap (*.srt)|*.srt
Vobsub (*.idx, *.sub)|*.idx;*.sub
```

### 4.1 .esr —— 工程文件（含全部参数与字幕数据）

### 4.2 SSA（Sub Station Alpha）—— 行格式逆向（反汇编 0x4074B4 区域）
写行循环结构（每字幕一条）：
```
Dialogue: <Start>,<End>,Default,NTP,0000,0000,0000,,<Text>
```
- 前缀串 `Dialogue: `（0x57958C，被 `mov eax,0x57958C` 引用）
- 尾部串 `,Default,NTP,0000,0000,0000,,`（0x5795A0）→ 样式名 `Default`（NTP 为前缀记号）、MarginL/R/V=0000、Effect 空
- 文件头串 `[Events]` + `Format: Marked,Start,End,Style,Name,Mar...`（0x579539，SSA v4 事件段头）
- 时间码格式：`%02d:%02d:%02d.%03d`（时:分:秒.毫秒，3 位毫秒）

### 4.3 SRT（SubRip）与 **SubRip with bitmap**（用户核心需求）
- 分隔符串 ` --> `（0x5795B0 区域）确认 SRT 时间行格式
- **"SubRip with bitmap"** 变体：时间轴（`序号 + start --> end`）+ 每字幕独立位图（`.bmp` 串在 0x5795C0 区域与 SRT 常量相邻）→ 输出**不含文本**，字幕图按时间轴分帧保存
- 时间码同样用 `%02d:%02d:%02d.%03d` 体系

### 4.4 VobSub（.idx + .sub）—— 模板逆向
idx 文件头与注释来自标准 VobSub 参考模板（.data 字符串）：
```
# VobSub index file, v7 (do not modify this line!)
# Custom colors (transp idxs and the four colors)
palette: 0,0,0,0, ... (16 项)
langidx: 0
# The original palette of the DVD in PGC#1
# Decomment next line to activate alternative name in DirectVobSub / WMP 6.x
# Force subtitle placement relative to (org.x, org.y)
```
- 默认四色调色板（Custom colors 注释旁）：**0, 0, ffffff, 000000**（透明/透明/白/黑），transp idxs `1110`
- .sub 位图 = **RLE 压缩**（VobSub 标准：颜色索引 2bit + 长度 6bit 的字节对编码），.idx 记录每字幕偏移（timestamp + filepos）

### 4.5 OCR 图片（Save OCR Image）
`.bmp`（Bitmap (*.bmp)|*.bmp），每字幕一张，可放大（Scale）、可拆 2 行（Divid）、每图字幕数可设（Subtitle Per Image）。

### 4.6 其他格式串（辅助功能）
- `%02d/%02d/%04d %02d:%02d:%02d.%03d`（0x586AE8，带毫秒文件时间戳 —— OCR 图片/日志命名）
- `\red%d\green%d\blue%d;`（RTF 颜色，富文本预览/导出）
- `border="%d" frame=box`、`padding-left:%dpx`（HTML/CSS，网页预览/导出样式）

---

## 5. 处理管线（综合反汇编 + 功能字典）

```
视频文件 ──DirectShow FilterGraph（COM 动态创建，无静态导入）──> 解码帧
    │
    ├─ Frame Skip（跳帧）── 每 N 帧取 1 帧
    ├─ Region 裁剪（Zoom/Full Width/上下左右）── 只抓字幕区
    ├─ Pixel Difference + Ignore Change(%) ── 帧间变化检测（字幕出现判定）
    │
    ▼
[三段色彩过滤]  Final(主色) / Pass1(轮廓) / Outline(描边)
    Hue Difference + RGB Difference + Lum/Sat 0-99 门限 + Pixel Compensate
    （Filter Method: Color 或 Color+Outline；Enable Filter 开关）
    │
    ▼
[后处理 5 种清理] 单点 / 单线 / 大块(>N) / 触边 / 跨中带
    │
    ▼
[字幕管理]  Force Merge / Better Quality / Recover / Remove / Crop / Purge
    │
    ▼
[输出]  .esr 工程 / .ssa / .srt / SubRip+bitmap(.srt+.bmp) / VobSub(.idx+.sub RLE) / OCR .bmp
    + 时间轴 Time Shift(10ms 步进) / Unicode 输出 / 样式四色
```

**时间轴语义**：字幕时间 = 帧号 / 帧率（DirectShow 媒体类型取帧率），可整体平移（Time Shift，10ms 粒度）。

---

## 6. 与 esrxp-ng（Rust 重构）实现对照

| esrXP 功能 | esrxp-ng 状态 | 说明 |
|---|---|---|
| Region 手动选区 | ✅ 已实现 | 预览拖拽框选，up/down/left/right |
| 三段色彩过滤（Hue/RGB/Lum/Sat 0-99 + 附加颜色） | ✅ 已实现 | filter.rs HSV+RGB 门限 |
| Enable Filter 开关 | ✅ 已实现 | enable_filter=false 仅主色 |
| 后处理 5 类清理 | ✅ 已实现 | postprocess.rs |
| Frame Skip / Pixel Difference / Ignore % | ✅ 已实现 | ripper.rs 抓取循环 |
| Force Merge / Better Quality | ✅ 已实现 | ripper.rs merge_repeat |
| SRT（无文本时间轴） | ✅ 已实现 | outputs.rs write_srt |
| SSA（样式四色/字体/非默认样式） | ✅ 已实现 | outputs.rs SSA 扩展 |
| VobSub .idx+.sub（RLE） | ✅ 已实现 | outputs.rs vobsub + mask_to_polygon |
| OCR 图片（.bmp，放大/拆行） | ✅ 已实现 | 输出 subtitle_imgs |
| Time Shift（10ms） | ✅ 已实现 | config.time_shift_10ms |
| Unicode 输出 | ✅ 已实现 | Rust UTF-8 原生 |
| .esr 工程文件 | 🔶 部分 | 输出工程 JSON（esrXP 原生 .esr 二进制格式未复刻，JSON 等价） |
| 批处理 | 🔶 未实现 | FBatch 待补 |
| 字幕管理器 UI（树视图/Recover/Remove/Crop/Purge） | 🔶 部分 | 数据层能力有，UI 管理页待补 |

---

## 7. 取证索引（地址表）

| 取证点 | 地址 | 证据 |
|---|---|---|
| 版本串 | — | `esrXP beta 10 (20051214)` |
| 注册表键 | 0x57920C | `esrXP\setting` |
| 注册表值名 | 0x5795D0 区 | `RipSubtitleColor` / `RipOutlineColor` |
| 保存过滤器 | 0x5E9D98（.rsrc DFM） | 全部 5 种输出格式 |
| SSA `Dialogue:` | 0x57958C | 前缀串（0x4074B4 mov 引用） |
| SSA 行尾 | 0x5795A0 | `,Default,NTP,0000,0000,0000,,` |
| SSA `[Events]` | 0x579539 | 事件段头 |
| SRT 分隔符 | 0x5795B0 区 | ` --> ` |
| 时间戳格式 | 0x586AE8 / 0x586AF7 | `%02d/%02d/%04d %02d:%02d:%02d.%03d` / `%02d:%02d:%02d.%03d` |
| 时间戳函数 | 0x571D08–0x571D55 | wsprintf 包装（0x571D45 call 0x578592） |
| SSA 写行函数 | 0x4074B4–0x4076B6 | Dialogue 行循环拼接 |
| VobSub 模板 | .data | `# VobSub index file, v7` / palette / langidx |
| 四色调色板 | .data | `custom colors: OFF, tridx: 1110, colors: 0, 0, ffffff, 000000` |
| DFM 阈值 | .rsrc | Lum/Sat/Hue UpDown 范围 0–99 |
| 组件 | RTTI | Graphics32 / Tnt / VirtualTree / TPerformanceGraph |
