//! 参数配置 UI —— 分组编辑（抓取/区域/过滤/后处理/输出(含OCR)/样式/预览）。
//! 序列化后与原 Vue UI 参数语义一致，字段名与后端 AppConfig 严格对齐。

use crate::config::*;
use eframe::egui::{self, DragValue, Slider};

pub fn rip_section(ui: &mut egui::Ui, cfg: &mut Rip, start: &mut f64, end: &mut f64) {
    egui::CollapsingHeader::new("抓取")
        .default_open(true)
        .show(ui, |ui| {
            ui.add(Slider::new(&mut cfg.frame_skip, 0..=10).text("影像跳读（每 N 帧取 1）"));
            ui.add(Slider::new(&mut cfg.pixel_difference, 0..=500).text("像点相差（下限）"));
            ui.add(Slider::new(&mut cfg.ignore_change_percent, 0.0..=5.0).text("忽略改变 %"));
            ui.add(Slider::new(&mut cfg.diff_threshold, 0..=255).text("变化判定阈值"));
            ui.add(Slider::new(&mut cfg.gap_frames, 0..=60).text("字幕分段帧数"));
            ui.checkbox(&mut cfg.force_merge, "强制合并相邻字幕");
            ui.checkbox(&mut cfg.better_quality, "更高质量（SSA 轮廓更细）");
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("起始时间");
                ui.add(DragValue::new(start).speed(0.5).suffix("s"));
            });
            ui.horizontal(|ui| {
                ui.label("结束时间（0=到片尾）");
                ui.add(DragValue::new(end).speed(0.5).suffix("s"));
            });
        });
}

pub fn region_section(
    ui: &mut egui::Ui,
    cfg: &mut Region,
    scale_video: &mut f64,
    sharpen_video: &mut bool,
) {
    egui::CollapsingHeader::new("区域")
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(Slider::new(&mut cfg.up, 0..=2000).text("上边距"));
                ui.add(Slider::new(&mut cfg.down, 0..=2000).text("下边距"));
            });
            ui.horizontal(|ui| {
                ui.add(Slider::new(&mut cfg.left, 0..=2000).text("左边距"));
                ui.add(Slider::new(&mut cfg.right, 0..=2000).text("右边距"));
            });
            ui.add(Slider::new(&mut cfg.scale, 0.5..=4.0).text("区域缩放"));
            ui.checkbox(&mut cfg.sharpen, "锐化区域");
            ui.separator();
            ui.add(Slider::new(scale_video, 0.25..=2.0).text("整帧缩放"));
            ui.checkbox(sharpen_video, "整帧锐化");
        });
}

fn rgb3(ui: &mut egui::Ui, label: &str, v: &mut (u8, u8, u8)) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(DragValue::new(&mut v.0).range(0..=255).speed(1));
        ui.add(DragValue::new(&mut v.1).range(0..=255).speed(1));
        ui.add(DragValue::new(&mut v.2).range(0..=255).speed(1));
    });
}

pub fn filter_section(ui: &mut egui::Ui, cfg: &mut Filter) {
    egui::CollapsingHeader::new("过滤 / 颜色")
        .default_open(false)
        .show(ui, |ui| {
            egui::ComboBox::from_id_salt("filter_method")
                .selected_text(cfg.method.as_str())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut cfg.method, "color".to_owned(), "color（单主色）");
                    ui.selectable_value(
                        &mut cfg.method,
                        "color_outline".to_owned(),
                        "color_outline（主色+描边）",
                    );
                });
            ui.checkbox(&mut cfg.enable_filter, "启用过滤（关掉仅保留主色）");
            rgb3(ui, "字幕颜色", &mut cfg.subtitle_color);
            rgb3(ui, "描边颜色", &mut cfg.outline_color);
            ui.add(Slider::new(&mut cfg.pixel_compensate, 0..=5).text("像点补偿（膨胀）"));
            ui.separator();
            ui.label(egui::RichText::new("附加颜色段").small());
            for (name, seg) in cfg.segments.iter_mut() {
                egui::CollapsingHeader::new(format!("{name}  RGB{:?} ±{}", seg.rgb, seg.rgb_diff))
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.checkbox(&mut seg.enable_rgb, "启用 RGB 匹配");
                        rgb3(ui, "目标色", &mut seg.rgb);
                        ui.add(Slider::new(&mut seg.rgb_diff, 0..=255).text("RGB 容差"));
                        ui.checkbox(&mut seg.enable_hue, "启用色相匹配");
                        ui.add(Slider::new(&mut seg.hue, 0..=360).text("色相"));
                        ui.add(Slider::new(&mut seg.hue_diff, 0..=180).text("色相容差"));
                    });
            }
            if cfg.additional_colors.is_empty() {
                ui.label(
                    egui::RichText::new("附加颜色（Additional Color）为空")
                        .weak()
                        .small(),
                );
            }
        });
}

pub fn postprocess_section(ui: &mut egui::Ui, cfg: &mut Postprocess) {
    egui::CollapsingHeader::new("后处理")
        .default_open(false)
        .show(ui, |ui| {
            ui.checkbox(&mut cfg.single_dot, "清除单点噪点");
            ui.checkbox(&mut cfg.single_line, "清除单线噪点");
            ui.add(Slider::new(&mut cfg.large_block, 0..=100).text("大块清除（0=关）"));
            ui.checkbox(&mut cfg.touch_edge, "保留贴边块");
            ui.checkbox(&mut cfg.pass_center, "保留过中心块");
            ui.add(Slider::new(&mut cfg.center_tolerance, 0.0..=1.0).text("中心容差"));
        });
}

pub fn ocr_section(ui: &mut egui::Ui, cfg: &mut Ocr) {
    ui.add(Slider::new(&mut cfg.scale, 0.5..=4.0).text("字幕缩放"));
    ui.add(Slider::new(&mut cfg.per_image, 1..=4).text("每图字幕条数"));
    ui.checkbox(&mut cfg.divid_into_2_lines, "两行拆上下两张");
    ui.separator();
    ui.label(egui::RichText::new("渲染画质").strong());
    ui.checkbox(&mut cfg.antialias, "边缘抗锯齿");
    ui.add_enabled(
        cfg.antialias,
        egui::Slider::new(&mut cfg.supersample, 1..=4).text("超采样 N×N"),
    );
    ui.add(Slider::new(&mut cfg.stroke_dilate, 0..=4).text("笔画加粗"));
    ui.add(Slider::new(&mut cfg.coverage_gamma, 0.3..=2.0).text("覆盖率先验"));
    ui.add(Slider::new(&mut cfg.binary_threshold, 0.05..=0.95).text("二值阈值"));
    ui.add(Slider::new(&mut cfg.despeckle_min_area, 0..=20).text("渲染前除噪"));
    egui::ComboBox::from_id_salt("ocr_color_mode")
        .selected_text(cfg.color_mode.as_str())
        .show_ui(ui, |ui| {
            for m in ["gray", "binary", "color"] {
                ui.selectable_value(&mut cfg.color_mode, m.to_owned(), m);
            }
        });
    hex_field(ui, "文字色", &mut cfg.text_color);
    hex_field(ui, "背景色", &mut cfg.bg_color);
    ui.separator();
    ui.label(egui::RichText::new("输出").strong());
    egui::ComboBox::from_id_salt("ocr_format")
        .selected_text(cfg.format.as_str())
        .show_ui(ui, |ui| {
            for f in ["png", "jpg", "bmp"] {
                ui.selectable_value(&mut cfg.format, f.to_owned(), f);
            }
        });
    ui.add_enabled(
        cfg.format == "jpg",
        Slider::new(&mut cfg.quality, 1..=100).text("JPG 质量"),
    );
    ui.add(Slider::new(&mut cfg.max_width, 0..=4000).text("最大宽度（0=不限）"));
    ui.separator();
    ui.label(egui::RichText::new("后处理").strong());
    ui.add(Slider::new(&mut cfg.padding, 0..=50).text("四周留边"));
    ui.horizontal(|ui| {
        ui.add(Slider::new(&mut cfg.crop_top, 0..=200).text("上裁"));
        ui.add(Slider::new(&mut cfg.crop_bottom, 0..=200).text("下裁"));
    });
    ui.horizontal(|ui| {
        ui.add(Slider::new(&mut cfg.crop_left, 0..=200).text("左裁"));
        ui.add(Slider::new(&mut cfg.crop_right, 0..=200).text("右裁"));
    });
    egui::ComboBox::from_id_salt("ocr_rotate")
        .selected_text(format!("旋转 {}", cfg.rotate))
        .show_ui(ui, |ui| {
            for r in [0, 90, 180, 270] {
                ui.selectable_value(&mut cfg.rotate, r, format!("旋转 {r}"));
            }
        });
    ui.horizontal(|ui| {
        ui.checkbox(&mut cfg.flip_h, "水平镜像");
        ui.checkbox(&mut cfg.flip_v, "垂直镜像");
    });
    ui.horizontal(|ui| {
        ui.add(Slider::new(&mut cfg.brightness, -100..=100).text("亮度"));
        ui.add(Slider::new(&mut cfg.contrast, -100..=100).text("对比度"));
    });
    ui.checkbox(&mut cfg.grayscale, "转为灰度");
}

fn hex_field(ui: &mut egui::Ui, label: &str, hex: &mut String) {
    let mut valid = true;
    let mut text = hex.clone();
    ui.horizontal(|ui| {
        ui.label(label);
        let resp = ui.add(egui::TextEdit::singleline(&mut text).desired_width(90.0));
        let cleaned: String = text
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .take(6)
            .collect();
        if cleaned.len() != 6 {
            valid = false;
        }
        if resp.changed() && valid {
            *hex = format!("#{}", cleaned.to_uppercase());
        }
    });
    if !valid {
        ui.label(egui::RichText::new("需为 #RRGGBB 格式").weak().small());
    }
}

pub fn output_section(ui: &mut egui::Ui, cfg: &mut Output) {
    egui::CollapsingHeader::new("输出")
        .default_open(false)
        .show(ui, |ui| {
            ui.checkbox(&mut cfg.ssa, "SSA（矢量轮廓）");
            ui.checkbox(&mut cfg.vobsub, "VobSub (.idx+.sub)");
            ui.checkbox(&mut cfg.ocr_png, "OCR 位图（PNG）");
            ui.checkbox(&mut cfg.srt, "SRT（时间轴）");
            ui.checkbox(&mut cfg.srt_bitmap, "SRT + 位图");
            ui.checkbox(&mut cfg.json_timeline, "JSON 时间轴");
            ui.add(Slider::new(&mut cfg.max_subtitle_width, 100..=4000).text("最大字幕宽度"));
            ui.separator();
            egui::CollapsingHeader::new("OCR 影像画质")
                .default_open(false)
                .show(ui, |ui| {
                    ocr_section(ui, &mut cfg.ocr);
                });
        });
}

pub fn style_section(ui: &mut egui::Ui, cfg: &mut Style) {
    egui::CollapsingHeader::new("样式")
        .default_open(false)
        .show(ui, |ui| {
            ui.text_edit_singleline(&mut cfg.font_name);
            ui.horizontal(|ui| {
                ui.add(Slider::new(&mut cfg.outline_width, 0..=10).text("描边宽"));
                ui.add(Slider::new(&mut cfg.shadow_depth, 0..=10).text("阴影深"));
            });
            ui.add(Slider::new(&mut cfg.time_shift_10ms, -1000..=1000).text("时间偏移（10ms）"));
            ui.checkbox(&mut cfg.unicode, "Unicode 风格");
            ui.checkbox(&mut cfg.no_default_style, "不使用默认样式");
            text_field(ui, "主色", &mut cfg.primary_color);
            text_field(ui, "次色", &mut cfg.secondary_color);
            text_field(ui, "描边色", &mut cfg.outline_color_ssa);
            text_field(ui, "阴影色", &mut cfg.shadow_color);
        });
}

fn text_field(ui: &mut egui::Ui, label: &str, s: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(s).desired_width(110.0));
    });
}

pub fn preview_section(ui: &mut egui::Ui, cfg: &mut Preview) {
    egui::CollapsingHeader::new("预览")
        .default_open(false)
        .show(ui, |ui| {
            ui.add(Slider::new(&mut cfg.scale_video, 0.25..=2.0).text("整帧缩放"));
            ui.checkbox(&mut cfg.sharpen_video, "整帧锐化");
            ui.checkbox(&mut cfg.white_background, "白背景");
            ui.add(Slider::new(&mut cfg.slow_speed, 0.1..=4.0).text("慢速倍率"));
        });
}

/// 渲染所有参数分组（左侧面板调用）
pub fn all_sections(ui: &mut egui::Ui, cfg: &mut UiConfig) {
    rip_section(
        ui,
        &mut cfg.rip,
        &mut cfg.start_seconds,
        &mut cfg.end_seconds,
    );
    region_section(
        ui,
        &mut cfg.region,
        &mut cfg.preview.scale_video,
        &mut cfg.preview.sharpen_video,
    );
    filter_section(ui, &mut cfg.filter);
    postprocess_section(ui, &mut cfg.postprocess);
    output_section(ui, &mut cfg.output);
    style_section(ui, &mut cfg.style);
    preview_section(ui, &mut cfg.preview);
}
