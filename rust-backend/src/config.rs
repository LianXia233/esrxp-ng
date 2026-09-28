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

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct OcrConfig {
    pub per_image: i64,
    pub scale: f64,
    pub divid_into_2_lines: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OutputConfig {
    pub ssa: bool,
    pub vobsub: bool,
    pub ocr_png: bool,
    pub srt: bool,                 // SRT：纯时间轴（不含文本，定位字幕图片出现区间）
    pub json_timeline: bool,
    pub ocr: OcrConfig,
    pub max_subtitle_width: i64,
    pub fps: Option<f64>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self { ssa: true, vobsub: true, ocr_png: true, srt: true, json_timeline: true,
               ocr: OcrConfig { per_image: 1, scale: 1.0, divid_into_2_lines: false },
               max_subtitle_width: 720, fps: None }
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
