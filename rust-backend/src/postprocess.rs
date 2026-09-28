//! 后处理 —— 连通域级清理（对应 esrXP 的 TFPostprocessingSetting）。
//! 两遍扫描 + 并查集标记，统计面积/bbox，再按配置过滤组件。

use crate::config::PostprocessConfig;

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
            let left = if x > 0 && mask[i - 1] > 0 { Some(labels[i - 1]) } else { None };
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
            let mut cands: Vec<i32> = [left, up, up_left, up_right].into_iter().flatten().collect();
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
    let cx = (w / 2) as i64;
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
            && (min_x[c] <= 0 || min_y[c] <= 0 || max_x[c] >= (w - 1) as i64 || max_y[c] >= (h - 1) as i64)
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
