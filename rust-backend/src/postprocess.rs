//! 后处理 —— 连通域级清理（对应 esrXP 的 TFPostprocessingSetting）。
//! 两遍扫描 + 并查集标记，统计面积/bbox，再按配置过滤组件。

use crate::config::PostprocessConfig;

/// 连通域级除噪：移除面积 ≤ `min_area` 的孤立小分量（椒盐噪声点/小簇）。
/// 用于 OCR / VobSub / 位图缩略图渲染前的二次清理——`clean` 只去掉单点与单线，
/// 面积 2-5px 的小噪点簇会残留，渲染成椒盐噪声。此函数补上这一档。
/// 返回 0/255 mask（非破坏：保留输入也可安全调用）。
pub fn despeckle(mask: &[u8], w: usize, h: usize, min_area: usize) -> Vec<u8> {
    let n = w * h;
    if n == 0 || min_area == 0 {
        return mask.to_vec();
    }
    let mut out = mask.to_vec();
    let mut labels = vec![0i32; n];
    let mut parent: Vec<i32> = vec![-1; 1];
    let mut next_label: i32 = 1;
    for y in 0..h {
        let row = y * w;
        let row_prev = if y > 0 { (y - 1) * w } else { 0 };
        for x in 0..w {
            let i = row + x;
            if mask[i] == 0 {
                continue;
            }
            let left = if x > 0 && mask[i - 1] > 0 {
                Some(labels[i - 1])
            } else {
                None
            };
            let up = if y > 0 && mask[row_prev + x] > 0 {
                Some(labels[row_prev + x])
            } else {
                None
            };
            let up_left = if y > 0 && x > 0 && mask[row_prev + x - 1] > 0 {
                Some(labels[row_prev + x - 1])
            } else {
                None
            };
            let up_right = if y > 0 && x + 1 < w && mask[row_prev + x + 1] > 0 {
                Some(labels[row_prev + x + 1])
            } else {
                None
            };
            let mut cands: Vec<i32> = [left, up, up_left, up_right]
                .into_iter()
                .flatten()
                .collect();
            if cands.is_empty() {
                parent.push(-1);
                labels[i] = next_label;
                next_label += 1;
            } else {
                cands.sort();
                cands.dedup();
                let mut root = find(&mut parent, cands[0]);
                for c in cands.iter().skip(1) {
                    let r2 = find(&mut parent, *c);
                    if r2 != root {
                        union(&mut parent, root, r2);
                        root = find(&mut parent, root);
                    }
                }
                labels[i] = root;
            }
        }
    }
    if next_label == 1 {
        return out;
    }
    let mut map: Vec<i32> = vec![0; parent.len()];
    let mut comps = 0;
    for i in 1..next_label {
        let r = find(&mut parent, i);
        if map[r as usize] == 0 {
            comps += 1;
            map[r as usize] = comps;
        }
    }
    let mut area = vec![0i64; (comps + 1) as usize];
    for l in labels.iter_mut() {
        if *l == 0 {
            continue;
        }
        let r = find(&mut parent, *l) as usize;
        *l = map[r];
        area[*l as usize] += 1;
    }
    // area/map/parent 均按「组件号」索引，遍历组件号是本算法的固有结构，
    // 改迭代器反而需要额外映射，故豁免 needless_range_loop。
    #[allow(clippy::needless_range_loop)]
    for c in 1..=(comps as usize) {
        if area[c] <= min_area as i64 {
            for (o, l) in out.iter_mut().zip(labels.iter()) {
                if *l as usize == c {
                    *o = 0;
                }
            }
        }
    }
    out
}

/// 对二值 mask（0/255）执行连通域清理，返回 0/255 mask。
pub fn clean(mask: &[u8], w: usize, h: usize, cfg: &PostprocessConfig) -> Vec<u8> {
    let n = w * h;
    if n == 0 {
        return Vec::new();
    }
    // --- 第一遍：游程标记 + 并查集 ---
    let mut labels = vec![0i32; n];
    let mut parent: Vec<i32> = vec![-1; 1]; // 并查集，根 parent<0 表示 -size
    let mut next_label: i32 = 1;
    for y in 0..h {
        let row = y * w;
        let row_prev = if y > 0 { (y - 1) * w } else { 0 };
        for x in 0..w {
            let i = row + x;
            if mask[i] == 0 {
                continue;
            }
            // 左邻 & 上邻（8 连通）
            let left = if x > 0 && mask[i - 1] > 0 {
                Some(labels[i - 1])
            } else {
                None
            };
            let up = if y > 0 && mask[row_prev + x] > 0 {
                Some(labels[row_prev + x])
            } else {
                None
            };
            let up_left = if y > 0 && x > 0 && mask[row_prev + x - 1] > 0 {
                Some(labels[row_prev + x - 1])
            } else {
                None
            };
            let up_right = if y > 0 && x + 1 < w && mask[row_prev + x + 1] > 0 {
                Some(labels[row_prev + x + 1])
            } else {
                None
            };
            let mut cands: Vec<i32> = [left, up, up_left, up_right]
                .into_iter()
                .flatten()
                .collect();
            if cands.is_empty() {
                parent.push(-1);
                labels[i] = next_label;
                next_label += 1;
            } else {
                cands.sort();
                cands.dedup();
                let mut root = find(&mut parent, cands[0]);
                for c in cands.iter().skip(1) {
                    let r2 = find(&mut parent, *c);
                    if r2 != root {
                        union(&mut parent, root, r2);
                        root = find(&mut parent, root);
                    }
                }
                labels[i] = root;
            }
        }
    }
    // 无前景像素（等价 Python `if num <= 1: return mask.copy()`）
    if next_label == 1 {
        return mask.to_vec();
    }
    // --- 压缩标签（0=未标记像素，不参与；标签从 1 起） ---
    let mut map: Vec<i32> = vec![0; parent.len()];
    let mut comps = 0;
    for i in 1..next_label {
        let r = find(&mut parent, i);
        if map[r as usize] == 0 {
            comps += 1;
            map[r as usize] = comps;
        }
    }
    // --- 统计组件 ---
    let mut area = vec![0i64; (comps + 1) as usize];
    let mut min_x = vec![w as i64; (comps + 1) as usize];
    let mut max_x = vec![0i64; (comps + 1) as usize];
    let mut min_y = vec![h as i64; (comps + 1) as usize];
    let mut max_y = vec![0i64; (comps + 1) as usize];
    for (i, l) in labels.iter_mut().enumerate() {
        if *l == 0 {
            continue;
        }
        let r = find(&mut parent, *l) as usize;
        *l = map[r];
        let c = *l as usize;
        let x = (i % w) as i64;
        let y = (i / w) as i64;
        area[c] += 1;
        min_x[c] = min_x[c].min(x);
        max_x[c] = max_x[c].max(x);
        min_y[c] = min_y[c].min(y);
        max_y[c] = max_y[c].max(y);
    }
    let band_y0 = (h as f64 * (0.5 - cfg.center_tolerance)) as i64;
    let band_y1 = (h as f64 * (0.5 + cfg.center_tolerance)) as i64;
    // --- 过滤输出 ---
    let mut out = vec![0u8; n];
    for c in 1..=(comps as usize) {
        let a = area[c];
        let bw = max_x[c] - min_x[c] + 1;
        let bh = max_y[c] - min_y[c] + 1;
        if a <= 0 {
            continue;
        }
        if cfg.single_dot && a == 1 {
            continue;
        }
        if cfg.single_line && a <= 4 && (bw == 1 || bh == 1) {
            continue;
        }
        if cfg.large_block > 0 && a > cfg.large_block {
            continue;
        }
        if cfg.touch_edge
            && (min_x[c] <= 0
                || min_y[c] <= 0
                || max_x[c] >= (w - 1) as i64
                || max_y[c] >= (h - 1) as i64)
        {
            continue;
        }
        if cfg.pass_center && min_y[c] < band_y1 && max_y[c] > band_y0 {
            // 完全跨中央带（上下都越界）→ 移除；部分跨 → 保留（对齐 Python 语义）
            if min_y[c] < band_y0 && max_y[c] > band_y1 {
                continue;
            }
        }
        for (i, l) in labels.iter().enumerate() {
            if *l as usize == c {
                out[i] = 255;
            }
        }
    }
    out
}

// parent 需在扫描中 push 扩容（见调用点），故保持 &mut Vec 而非切片。
#[allow(clippy::ptr_arg)]
fn find(parent: &mut Vec<i32>, mut i: i32) -> i32 {
    let mut root = i;
    while parent[root as usize] >= 0 {
        root = parent[root as usize];
    }
    while i != root {
        let p = parent[i as usize];
        parent[i as usize] = root;
        i = p;
    }
    root
}

#[allow(clippy::ptr_arg)]
fn union(parent: &mut Vec<i32>, a: i32, b: i32) {
    if a == b {
        return;
    }
    let (mut a, mut b) = (a, b);
    if parent[a as usize] > parent[b as usize] {
        std::mem::swap(&mut a, &mut b);
    }
    parent[a as usize] += parent[b as usize];
    parent[b as usize] = a;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_from_str(rows: &[&str]) -> Vec<u8> {
        let h = rows.len();
        let w = rows[0].len();
        let mut m = vec![0u8; w * h];
        for (y, r) in rows.iter().enumerate() {
            for (x, ch) in r.chars().enumerate() {
                m[y * w + x] = if ch == '#' { 255 } else { 0 };
            }
        }
        m
    }

    #[test]
    fn despeckle_keeps_large_component_removes_small() {
        // 大字块（面积 6）+ 两个小噪点簇（面积 2、3）
        let m = mask_from_str(&[
            "..##..", "..##..", "..#...", "..#...", "......", "..#...", ".....#",
        ]);
        // 面积：竖线 6、第5行单点 1、右下角单点 1 → despeckle(4) 应全部保留竖线
        let out = despeckle(&m, 6, 7, 4);
        let nz = out.iter().filter(|v| **v > 0).count();
        assert_eq!(nz, 6, "只有面积>4 的主组件应保留");
        // 放大阈值 → 竖线(6)也被清掉，与语义一致（min_area 为「面积≤则移除」）
        let out2 = despeckle(&m, 6, 7, 6);
        assert_eq!(out2.iter().filter(|v| **v > 0).count(), 0);
    }

    #[test]
    fn despeckle_zero_returns_copy() {
        let m = vec![255u8; 12];
        let out = despeckle(&m, 4, 3, 0);
        assert_eq!(out, m);
    }
}
