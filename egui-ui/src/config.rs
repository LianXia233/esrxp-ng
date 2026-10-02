//! 配置模型镜像 —— 与 rust-backend::config 的 JSON 序列化契约严格对齐。
//! 不依赖后端 crate，避免 GUI 构建被 ffmpeg 绑定拖重；字段名/结构与后端 AppConfig 一致。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub rip: Rip,
    pub region: Region,
    pub filter: Filter,
    pub postprocess: Postprocess,
    pub style: Style,
    pub output: Output,
    pub preview: Preview,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub verbose: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            rip: Rip::default(),
            region: Region::default(),
            filter: Filter::default(),
            postprocess: Postprocess::default(),
            style: Style::default(),
            output: Output::default(),
            preview: Preview::default(),
            start_seconds: 0.0,
            end_seconds: 0.0,
            verbose: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Rip {
    pub frame_skip: i64,
    pub pixel_difference: i64,
    pub ignore_change_percent: f64,
    pub diff_threshold: i64,
    pub gap_frames: i64,
    pub force_merge: bool,
    pub better_quality: bool,
}

impl Default for Rip {
    fn default() -> Self {
        Self {
            frame_skip: 1,
            pixel_difference: 20,
            ignore_change_percent: 0.5,
            diff_threshold: 24,
            gap_frames: 5,
            force_merge: false,
            better_quality: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Region {
    pub up: i64,
    pub down: i64,
    pub left: i64,
    pub right: i64,
    pub scale: f64,
    pub sharpen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        Self {
            hue: 0,
            hue_diff: 10,
            rgb: (255, 255, 255),
            rgb_diff: 40,
            lum_min: 0,
            lum_max: 255,
            sat_min: 0,
            sat_max: 255,
            enable_hue: false,
            enable_rgb: true,
            enable_lum_min: false,
            enable_lum_max: false,
            enable_sat_min: false,
            enable_sat_max: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    pub method: String,
    pub subtitle_color: (u8, u8, u8),
    pub outline_color: (u8, u8, u8),
    pub pixel_compensate: i64,
    pub enable_filter: bool,
    pub additional_colors: Vec<(u8, u8, u8)>,
    pub segments: BTreeMap<String, ColorSegment>,
}

impl Default for Filter {
    fn default() -> Self {
        let mut segments = BTreeMap::new();
        segments.insert(
            "outline".into(),
            ColorSegment {
                rgb: (0, 0, 0),
                rgb_diff: 30,
                ..Default::default()
            },
        );
        segments.insert(
            "pass1".into(),
            ColorSegment {
                rgb: (255, 255, 255),
                ..Default::default()
            },
        );
        segments.insert(
            "final".into(),
            ColorSegment {
                rgb: (255, 255, 255),
                ..Default::default()
            },
        );
        Self {
            method: "color_outline".into(),
            subtitle_color: (255, 255, 255),
            outline_color: (0, 0, 0),
            pixel_compensate: 1,
            enable_filter: true,
            additional_colors: vec![],
            segments,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Postprocess {
    pub single_dot: bool,
    pub single_line: bool,
    pub large_block: i64,
    pub touch_edge: bool,
    pub pass_center: bool,
    pub center_tolerance: f64,
}

impl Default for Postprocess {
    fn default() -> Self {
        Self {
            single_dot: true,
            single_line: true,
            large_block: 0,
            touch_edge: false,
            pass_center: true,
            center_tolerance: 0.12,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub unicode: bool,
    pub time_shift_10ms: i64,
    pub outline_width: i64,
    pub shadow_depth: i64,
    pub primary_color: String,
    pub secondary_color: String,
    pub outline_color_ssa: String,
    pub shadow_color: String,
    pub font_name: String,
    pub no_default_style: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            unicode: true,
            time_shift_10ms: 0,
            outline_width: 1,
            shadow_depth: 0,
            primary_color: "&H00FFFFFF&".into(),
            secondary_color: "&H000000FF&".into(),
            outline_color_ssa: "&H00000000&".into(),
            shadow_color: "&H00000000&".into(),
            font_name: "Arial".into(),
            no_default_style: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Ocr {
    pub per_image: i64,
    pub scale: f64,
    pub divid_into_2_lines: bool,
    pub color_mode: String,
    pub antialias: bool,
    pub supersample: i64,
    pub stroke_dilate: i64,
    pub coverage_gamma: f64,
    pub binary_threshold: f64,
    pub despeckle_min_area: i64,
    pub text_color: String,
    pub bg_color: String,
    pub format: String,
    pub quality: i64,
    pub scale_filter: String,
    pub max_width: i64,
    pub padding: i64,
    pub crop_top: i64,
    pub crop_bottom: i64,
    pub crop_left: i64,
    pub crop_right: i64,
    pub rotate: i64,
    pub flip_h: bool,
    pub flip_v: bool,
    pub brightness: i64,
    pub contrast: i64,
    pub grayscale: bool,
}

impl Default for Ocr {
    fn default() -> Self {
        Self {
            per_image: 1,
            scale: 1.0,
            divid_into_2_lines: false,
            color_mode: "gray".into(),
            antialias: true,
            supersample: 3,
            stroke_dilate: 0,
            coverage_gamma: 0.85,
            binary_threshold: 0.5,
            despeckle_min_area: 4,
            text_color: "#000000".into(),
            bg_color: "#FFFFFF".into(),
            format: "png".into(),
            quality: 95,
            scale_filter: "lanczos3".into(),
            max_width: 0,
            padding: 0,
            crop_top: 0,
            crop_bottom: 0,
            crop_left: 0,
            crop_right: 0,
            rotate: 0,
            flip_h: false,
            flip_v: false,
            brightness: 0,
            contrast: 0,
            grayscale: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Output {
    pub ssa: bool,
    pub vobsub: bool,
    pub ocr_png: bool,
    pub srt: bool,
    pub srt_bitmap: bool,
    pub json_timeline: bool,
    pub ocr: Ocr,
    pub max_subtitle_width: i64,
    pub fps: Option<f64>,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            ssa: true,
            vobsub: true,
            ocr_png: true,
            srt: true,
            srt_bitmap: false,
            json_timeline: true,
            ocr: Ocr::default(),
            max_subtitle_width: 720,
            fps: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preview {
    pub scale_video: f64,
    pub sharpen_video: bool,
    pub white_background: bool,
    pub slow_speed: f64,
}

impl Default for Preview {
    fn default() -> Self {
        Self {
            scale_video: 1.0,
            sharpen_video: false,
            white_background: false,
            slow_speed: 1.0,
        }
    }
}

/// 预览模式列表（与后端 /api/preview 的 mode 一致）
pub const PREVIEW_MODES: [&str; 5] = ["raw", "mask", "overlay", "combo", "ocr"];
