//! 色彩过滤 —— 忠实还原 esrXP 的三段判据（Outline / Pass1 / Final）。
//!
//! 每段判据：Hue 环形距离 / RGB 平方欧氏距离（int32，防溢出） / 亮度范围 /
//! 饱和度范围，通道独立使能、段内 AND、段间 OR；像素补偿用 3x3 膨胀。
//! 自动选色：变化像素亮度分位（最亮 15% 中位=主色，最暗 15% 为描边）。
//!
//! HSV 采用整数公式（H 0..359、S/V 0..255），与 GPU(CUDA PTX) 内核逐位一致。

use crate::config::{ColorSegment, FilterConfig};

/// 整数 RGB→HSV：H 0..359, S/V 0..255。
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (i64, i64, i64) {
    let (r, g, b) = (r as i64, g as i64, b as i64);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let mut h = if d == 0 {
        0
    } else if r == mx {
        ((g - b) * 60) / d
    } else if g == mx {
        ((b - r) * 60) / d + 120
    } else {
        ((r - g) * 60) / d + 240
    };
    if h < 0 {
        h += 360;
    }
    let s = if mx == 0 { 0 } else { d * 255 / mx };
    (h, s, mx)
}

/// 计算一段判据的命中 mask（bool，w*h）。
pub fn segment_mask(rgb: &[u8], w: usize, h: usize, seg: &ColorSegment) -> Vec<bool> {
    let mut mask = vec![true; w * h];
    let need_hsv = seg.enable_hue
        || seg.enable_lum_min
        || seg.enable_lum_max
        || seg.enable_sat_min
        || seg.enable_sat_max;
    let hsv: Option<Vec<(i64, i64, i64)>> = if need_hsv {
        Some(
            rgb.as_chunks::<3>()
                .0
                .iter()
                .map(|p| rgb_to_hsv(p[0], p[1], p[2]))
                .collect(),
        )
    } else {
        None
    };
    let t = seg.rgb;
    let rd2 = seg.rgb_diff * seg.rgb_diff;
    let hue_ref = seg.hue % 180;
    for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
        if seg.enable_rgb {
            let dr = px[0] as i64 - t.0 as i64;
            let dg = px[1] as i64 - t.1 as i64;
            let db = px[2] as i64 - t.2 as i64;
            if dr * dr + dg * dg + db * db > rd2 {
                mask[i] = false;
            }
        }
        if let Some(ref hsv) = hsv {
            let (hh, ss, vv) = hsv[i];
            let hue180 = hh / 2;
            if seg.enable_hue {
                let d = (hue180 - hue_ref).abs();
                if d.min(180 - d) > seg.hue_diff {
                    mask[i] = false;
                }
            }
            if seg.enable_lum_min && vv < seg.lum_min {
                mask[i] = false;
            }
            if seg.enable_lum_max && vv > seg.lum_max {
                mask[i] = false;
            }
            if seg.enable_sat_min && ss < seg.sat_min {
                mask[i] = false;
            }
            if seg.enable_sat_max && ss > seg.sat_max {
                mask[i] = false;
            }
        }
    }
    mask
}

/// 当前生效的段（供 GPU 段参数上传复用）。
pub fn active_segments_pub(cfg: &FilterConfig) -> Vec<ColorSegment> {
    let default = ColorSegment::default();
    if cfg.method == "color_outline" {
        vec![
            cfg.segments
                .get("outline")
                .cloned()
                .unwrap_or_else(|| default.clone()),
            cfg.segments
                .get("final")
                .cloned()
                .unwrap_or_else(|| default.clone()),
        ]
    } else if cfg
        .segments
        .get("pass1")
        .map(|s| s.enabled())
        .unwrap_or(false)
        && !cfg
            .segments
            .get("final")
            .map(|s| s.enabled())
            .unwrap_or(false)
    {
        vec![cfg.segments.get("pass1").cloned().unwrap_or(default)]
    } else {
        vec![cfg.segments.get("final").cloned().unwrap_or(default)]
    }
}

/// 3x3 椭圆膨胀（二值 0/255）。
pub fn dilate3(mask: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            if mask[y * w + x] > 0 {
                let (x0, x1) = (x.saturating_sub(1), (x + 1).min(w - 1));
                let (y0, y1) = (y.saturating_sub(1), (y + 1).min(h - 1));
                for yy in y0..=y1 {
                    for xx in x0..=x1 {
                        out[yy * w + xx] = 255;
                    }
                }
            }
        }
    }
    out
}

/// 整帧过滤（CPU）：段 OR + 像素补偿膨胀。
pub fn filter_frame_cpu(rgb: &[u8], w: usize, h: usize, cfg: &FilterConfig) -> Vec<u8> {
    let mut combined = vec![0u8; w * h];
    for seg in active_segments_pub(cfg) {
        let m = segment_mask(rgb, w, h, &seg);
        for (i, hit) in m.iter().enumerate() {
            if *hit {
                combined[i] = 255;
            }
        }
    }
    let n = cfg.pixel_compensate.max(0);
    let mut cur = combined;
    for _ in 0..n {
        cur = dilate3(&cur, w, h);
    }
    cur
}

/// 帧差统计（CPU）：任一路径通道差 > threshold 视为变化。
pub fn frame_diff_cpu(prev: &[u8], cur: &[u8], threshold: i64) -> (i64, f64, Vec<bool>) {
    let n = prev.len() / 3;
    let mut changed = 0i64;
    let mut cmask = vec![false; n];
    for i in 0..n {
        let p = &prev[i * 3..i * 3 + 3];
        let c = &cur[i * 3..i * 3 + 3];
        let d = (p[0] as i64 - c[0] as i64)
            .abs()
            .max((p[1] as i64 - c[1] as i64).abs())
            .max((p[2] as i64 - c[2] as i64).abs());
        if d > threshold {
            changed += 1;
            cmask[i] = true;
        }
    }
    let ratio = if n > 0 {
        changed as f64 / n as f64
    } else {
        0.0
    };
    (changed, ratio, cmask)
}

/// 最近邻缩放（CPU）。
pub fn scale_nearest_cpu(rgb: &[u8], w: usize, h: usize, factor: f64) -> (Vec<u8>, usize, usize) {
    if factor <= 0.0 {
        return (rgb.to_vec(), w, h);
    }
    let (nw, nh) = ((w as f64 * factor) as usize, (h as f64 * factor) as usize);
    if nw == 0 || nh == 0 {
        return (rgb.to_vec(), w, h);
    }
    if nw == w && nh == h {
        return (rgb.to_vec(), w, h);
    }
    let mut out = vec![0u8; nw * nh * 3];
    for y in 0..nh {
        let sy = ((y as f64) / factor).min(h as f64 - 1.0) as usize;
        for x in 0..nw {
            let sx = ((x as f64) / factor).min(w as f64 - 1.0) as usize;
            let (di, si) = ((y * nw + x) * 3, (sy * w + sx) * 3);
            out[di..di + 3].copy_from_slice(&rgb[si..si + 3]);
        }
    }
    (out, nw, nh)
}

/// 自动估计字幕色与描边色（亮度分位法）。
pub fn auto_detect_colors(
    rgb: &[u8],
    diff_mask: &[bool],
    _w: usize,
    _h: usize,
) -> ((u8, u8, u8), (u8, u8, u8)) {
    let mut pts: Vec<(u32, [u8; 3])> = Vec::new();
    for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
        if i < diff_mask.len() && diff_mask[i] {
            let lum = px[0] as u32 + px[1] as u32 + px[2] as u32;
            pts.push((lum, [px[0], px[1], px[2]]));
        }
    }
    if pts.len() < 16 {
        return ((255, 255, 255), (0, 0, 0));
    }
    pts.sort_by_key(|p| p.0);
    let n = pts.len();
    let hi = &pts[n * 85 / 100..];
    let lo = &pts[..(n * 15 / 100).max(1)];
    let main = median_rgb(hi);
    let outline = median_rgb(lo);
    (main, outline)
}

fn median_rgb(pts: &[(u32, [u8; 3])]) -> (u8, u8, u8) {
    let n = pts.len();
    let mut rs: Vec<u8> = pts.iter().map(|p| p.1[0]).collect();
    let mut gs: Vec<u8> = pts.iter().map(|p| p.1[1]).collect();
    let mut bs: Vec<u8> = pts.iter().map(|p| p.1[2]).collect();
    rs.sort();
    gs.sort();
    bs.sort();
    (rs[n / 2], gs[n / 2], bs[n / 2])
}

/// 颜色合理性门控：主色亮、描边暗。
pub fn colors_plausible(main: (u8, u8, u8), outline: (u8, u8, u8)) -> bool {
    (main.0 as f64 + main.1 as f64 + main.2 as f64) / 3.0 >= 155.0
        && (outline.0 as f64 + outline.1 as f64 + outline.2 as f64) / 3.0 <= 95.0
}

/// mask 合理性：非空且覆盖 < 30%。
/// 用浮点比较而非 `count < area * 30 / 100`：整数除法在 ROI 很小时会把阈值截断到 0
/// （如 area=3 时阈值 0，任何非空 mask 都判为不合理），且 30% 整恰好被误判为不合理。
pub fn mask_plausible(mask: &[u8], area: usize) -> bool {
    let count = mask.iter().filter(|v| **v > 0).count();
    if count == 0 || area == 0 {
        return false;
    }
    (count as f64) / (area as f64) < 0.30
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_of(n: usize, total: usize) -> Vec<u8> {
        let mut m = vec![0u8; total];
        for v in m.iter_mut().take(n) {
            *v = 255;
        }
        m
    }

    /// 小 ROI 不得因整数截断把「本应合理」的 mask 判为不合理。
    /// 原实现 `count < area*30/100` 在 area=4 时阈值退化为 1，
    /// 1 像素前景（1 < 1 为假）被误拒，而 1/4 = 25% 本应合理。
    #[test]
    fn mask_plausible_small_area_not_truncated() {
        assert!(mask_plausible(&mask_of(1, 4), 4), "1/4 = 25% < 30% 应合理");
    }

    /// 面积很小时按真实比例判定：1/3 > 30% 故不合理，但 1/4 = 25% 应合理。
    #[test]
    fn mask_plausible_uses_real_ratio() {
        assert!(mask_plausible(&mask_of(1, 4), 4), "1/4 = 25% < 30% 应合理");
        assert!(
            !mask_plausible(&mask_of(1, 3), 3),
            "1/3 ≈ 33.3% > 30% 应不合理"
        );
    }

    #[test]
    fn mask_plausible_rejects_empty_and_full() {
        assert!(!mask_plausible(&[0u8; 16], 16), "全空应不合理");
        assert!(!mask_plausible(&[255u8; 16], 16), "100% 覆盖应不合理");
        assert!(!mask_plausible(&[255u8; 4], 0), "area=0 应不合理");
    }

    /// 30% 临界：整数实现会误判，浮点实现按注释语义「< 30%」处理。
    #[test]
    fn mask_plausible_boundary_is_strict() {
        assert!(
            !mask_plausible(&mask_of(30, 100), 100),
            "恰好 30% 不满足 < 30%"
        );
        assert!(mask_plausible(&mask_of(29, 100), 100), "29% 应合理");
    }
}
