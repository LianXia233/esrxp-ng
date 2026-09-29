//! 配置模型 —— 与 Python 侧 config JSON 完全兼容（对应 esrXP 注册表 schema）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RipConfig {
    pub frame_skip: i64,           // 影像跳读：每 N 帧取 1 帧
    pub pixel_difference: i64,     // 像点相差：变化像素数下限
    pub ignore_change_percent: f64,// 忽略改变 %：变化占比下限
    pub diff_threshold: i64,       // 单像素变化判定阈值
    pub gap_frames: i64,           // 字幕分段帧数
    pub force_merge: bool,         // 强制合并（字幕管理器 Force Merge：相邻同区字幕强制并为一条）
    pub better_quality: bool,      // 更高质量（Better Quality：SSA 轮廓保留更多细节）
}

impl Default for RipConfig {
    fn default() -> Self {
        Self { frame_skip: 1, pixel_difference: 20, ignore_change_percent: 0.5,
               diff_threshold: 24, gap_frames: 5, force_merge: false, better_quality: false }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct RegionConfig {
    pub up: i64,
    pub down: i64,
    pub left: i64,
    pub right: i64,
    pub scale: f64,
    pub sharpen: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ColorSegment {
    pub hue: i64,
    pub hue_diff: i64,
    pub rgb: (u8, u8, u8),
    pub rgb_diff: i64,
    pub lum_min: i64,
    pub lum_max: i64,
    pub sat_min: i64,
    pub sat_max: i64,
    pub enable_hue: bool,
    pub enable_rgb: bool,
    pub enable_lum_min: bool,
    pub enable_lum_max: bool,
    pub enable_sat_min: bool,
    pub enable_sat_max: bool,
}

impl Default for ColorSegment {
    fn default() -> Self {
        Self { hue: 0, hue_diff: 10, rgb: (255, 255, 255), rgb_diff: 40,
               lum_min: 0, lum_max: 255, sat_min: 0, sat_max: 255,
               enable_hue: false, enable_rgb: true,
               enable_lum_min: false, enable_lum_max: false,
               enable_sat_min: false, enable_sat_max: false }
    }
}

impl ColorSegment {
    pub fn enabled(&self) -> bool {
        self.enable_hue || self.enable_rgb || self.enable_lum_min
            || self.enable_lum_max || self.enable_sat_min || self.enable_sat_max
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FilterConfig {
    pub method: String,              // "color" | "color_outline"
    pub subtitle_color: (u8, u8, u8),
    pub outline_color: (u8, u8, u8),
    pub pixel_compensate: i64,       // 像点补偿（膨胀次数）
    pub enable_filter: bool,         // Enable Filter：关掉时仅保留主色过滤
    pub additional_colors: Vec<(u8, u8, u8)>, // Additional Color：附加颜色（自动生成额外颜色段）
    pub segments: std::collections::BTreeMap<String, ColorSegment>,
}

impl Default for FilterConfig {
    fn default() -> Self {
        let mut outline = ColorSegment::default();
        outline.rgb = (0, 0, 0);
        outline.rgb_diff = 30;
        let mut final_seg = ColorSegment::default();
        final_seg.rgb = (255, 255, 255);
        let mut pass1 = ColorSegment::default();
        pass1.rgb = (255, 255, 255);
        let mut segs = std::collections::BTreeMap::new();
        segs.insert("outline".into(), outline);
        segs.insert("pass1".into(), pass1);
        segs.insert("final".into(), final_seg);
        Self { method: "color_outline".into(), subtitle_color: (255, 255, 255),
               outline_color: (0, 0, 0), pixel_compensate: 1, enable_filter: true,
               additional_colors: vec![], segments: segs }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PostprocessConfig {
    pub single_dot: bool,
    pub single_line: bool,
    pub large_block: i64,
    pub touch_edge: bool,
    pub pass_center: bool,
    pub center_tolerance: f64,
}

impl Default for PostprocessConfig {
    fn default() -> Self {
        Self { single_dot: true, single_line: true, large_block: 0,
               touch_edge: false, pass_center: true, center_tolerance: 0.12 }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct StyleConfig {
    pub unicode: bool,
    pub time_shift_10ms: i64,
    pub outline_width: i64,
    pub shadow_depth: i64,
    pub primary_color: String,
    pub secondary_color: String,   // Subtitle Style: Secondary
    pub outline_color_ssa: String,
    pub shadow_color: String,      // Subtitle Style: Shadow
    pub font_name: String,         // Select Font
    pub no_default_style: bool,    // No Default Subtitle Style
}

impl Default for StyleConfig {
    fn default() -> Self {
        Self { unicode: true, time_shift_10ms: 0, outline_width: 1, shadow_depth: 0,
               primary_color: "&H00FFFFFF&".into(), secondary_color: "&H000000FF&".into(),
               outline_color_ssa: "&H00000000&".into(), shadow_color: "&H00000000&".into(),
               font_name: "Arial".into(), no_default_style: false }
    }
}

/// OCR 影像 / 字幕截图导出控制（对齐 esrXP [FOCRImage]，0.5.0 扩展画质与后处理项）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OcrConfig {
    pub per_image: i64,           // Subtitle Per Image：每张图容纳的字幕条数
    pub scale: f64,               // Scale Subtitle：缩放比例
    pub divid_into_2_lines: bool, // Divid 2 lines：两行字幕拆成上下两张
    // —— 渲染画质（决定「是否清晰」的关键） ——
    pub color_mode: String,       // gray(默认,前景/背景两色平滑混合) / binary(旧二值硬边) / color(保留原视频色)
    pub antialias: bool,          // 边缘抗锯齿：mask 覆盖率重建，关闭即回到硬边
    pub supersample: i64,         // 抗锯齿采样数 N（每像素 N×N 次采样，1-4）
    pub text_color: String,       // 前景（文字）色 #RRGGBB
    pub bg_color: String,         // 背景色 #RRGGBB
    // —— 输出格式与质量 ——
    pub format: String,           // png / jpg / bmp
    pub quality: i64,             // JPG 质量 1-100（png/bmp 为无损，忽略该项）
    pub scale_filter: String,     // 重采样滤镜：nearest / triangle / catmullrom / lanczos3
    pub max_width: i64,           // 输出最大宽度限制（0 = 不限）
    // —— 后处理 ——
    pub padding: i64,             // 四周留边（输出像素）
    pub crop_top: i64,            // 上边裁剪（输出像素）
    pub crop_bottom: i64,         // 下边裁剪
    pub crop_left: i64,           // 左边裁剪
    pub crop_right: i64,          // 右边裁剪
    pub rotate: i64,              // 旋转：0 / 90 / 180 / 270
    pub flip_h: bool,             // 水平镜像
    pub flip_v: bool,             // 垂直镜像
    pub brightness: i64,          // 亮度：-100..100
    pub contrast: i64,            // 对比度：-100..100
    pub grayscale: bool,          // 转为灰度图
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self { per_image: 1, scale: 1.0, divid_into_2_lines: false,
               color_mode: "gray".into(), antialias: true, supersample: 3,
               text_color: "#000000".into(), bg_color: "#FFFFFF".into(),
               format: "png".into(), quality: 95,
               scale_filter: "lanczos3".into(), max_width: 0,
               padding: 0, crop_top: 0, crop_bottom: 0, crop_left: 0, crop_right: 0,
               rotate: 0, flip_h: false, flip_v: false,
               brightness: 0, contrast: 0, grayscale: false }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OutputConfig {
    pub ssa: bool,
    pub vobsub: bool,
    pub ocr_png: bool,
    pub srt: bool,                 // SRT：纯时间轴（不含文本，定位字幕图片出现区间）
    pub srt_bitmap: bool,          // SubRip with bitmap：SRT 时间轴 + 每字幕独立位图
    pub json_timeline: bool,
    pub ocr: OcrConfig,
    pub max_subtitle_width: i64,
    pub fps: Option<f64>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self { ssa: true, vobsub: true, ocr_png: true, srt: true, srt_bitmap: false,
               json_timeline: true,
               ocr: OcrConfig::default(),
               max_subtitle_width: 720, fps: None }
    }
}

/// 预览/预处理控制 —— 对应 esrXP [FMain] 的 Scale Video / Sharpen Video /
/// White Background / Slow Speed。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PreviewConfig {
    pub scale_video: f64,      // Scale Video：整帧缩放（乘入 region.scale）
    pub sharpen_video: bool,   // Sharpen Video：整帧锐化（并入 region.sharpen）
    pub white_background: bool,// White Background：预览/输出背景置白
    pub slow_speed: f64,       // Slow Speed：预览慢速（前端逐帧步进倍率）
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self { scale_video: 1.0, sharpen_video: false, white_background: false, slow_speed: 1.0 }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub rip: RipConfig,
    pub region: RegionConfig,
    pub filter: FilterConfig,
    pub postprocess: PostprocessConfig,
    pub style: StyleConfig,
    pub output: OutputConfig,
    pub preview: PreviewConfig,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub verbose: bool,
}

impl AppConfig {
    pub fn default_json() -> String {
        serde_json::to_string_pretty(&Self::default()).unwrap()
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}
