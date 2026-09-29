//! GPU 加速层 —— 优先 CUDA（用户硬约束），自动回退 CPU。
//!
//! 分层设计：
//!   - `FrameKernels` trait：过滤/帧差/缩放/膨胀的运算接口
//!   - `CudaKernels`：NVIDIA CUDA 实现（动态加载 libcuda.so.1 / nvcuda.dll +
//!     内嵌 PTX 内核，零 CUDA 工具链依赖；Win11 下 DX 驱动即插即用）
//!   - `CpuKernels`：Rust 软件实现（沙箱/无 NVIDIA 环境，也是正确性基准）
//!   - `kernels()`：进程内单例，运行时探测（CUDA 可用→GPU，否则→CPU）
//!
//! 数值一致性：CPU/GPU 共用整数 HSV 与 int32 平方距离（RGB 平方距离必须
//! int32，int16 会溢出造成全屏误命中）。

use std::ffi::c_void;
use std::sync::OnceLock;

use libloading::{Library, Symbol};

use crate::config::FilterConfig;

// ================================================================ 接口
pub trait FrameKernels: Send + Sync {
    /// 过滤：RGB24 → mask（0/255），含像素补偿（3x3 膨胀 n 次）。
    fn filter(&self, rgb: &[u8], w: usize, h: usize, cfg: &FilterConfig) -> Vec<u8>;
    /// 帧差：返回 (变化像素数, 比例, 变化 mask)。
    fn frame_diff(&self, prev: &[u8], cur: &[u8], threshold: i64) -> (i64, f64, Vec<bool>);
    /// 最近邻缩放：返回 (新 rgb, nw, nh)。
    fn scale(&self, rgb: &[u8], w: usize, h: usize, factor: f64) -> (Vec<u8>, usize, usize);
}

pub fn kernels() -> &'static dyn FrameKernels {
    static K: OnceLock<Box<dyn FrameKernels>> = OnceLock::new();
    let b = K.get_or_init(|| match CudaKernels::probe() {
        Some(c) => Box::new(c) as Box<dyn FrameKernels>,
        None => Box::new(CpuKernels) as Box<dyn FrameKernels>,
    });
    b.as_ref()
}

/// 当前后端名（日志/UI 展示）。
pub fn backend_name() -> &'static str {
    static NAME: OnceLock<&'static str> = OnceLock::new();
    *NAME.get_or_init(|| {
        if CudaKernels::probe().is_some() {
            "CUDA"
        } else {
            "CPU"
        }
    })
}

// ================================================================ CPU 实现
pub struct CpuKernels;

impl FrameKernels for CpuKernels {
    fn filter(&self, rgb: &[u8], w: usize, h: usize, cfg: &FilterConfig) -> Vec<u8> {
        crate::filter::filter_frame_cpu(rgb, w, h, cfg)
    }
    fn frame_diff(&self, prev: &[u8], cur: &[u8], threshold: i64) -> (i64, f64, Vec<bool>) {
        crate::filter::frame_diff_cpu(prev, cur, threshold)
    }
    fn scale(&self, rgb: &[u8], w: usize, h: usize, factor: f64) -> (Vec<u8>, usize, usize) {
        crate::filter::scale_nearest_cpu(rgb, w, h, factor)
    }
}

// ================================================================ CUDA 实现
pub struct CudaKernels {
    _lib: Library,              // 保持驱动句柄存活
    ctx: CUcontext,
    mod_: CUmodule,
    f_filter: CUfunction,
    f_dilate: CUfunction,
    f_diff: CUfunction,
    f_scale: CUfunction,
    segbuf: CUdeviceptr,        // 段参数 device 内存（16 u32 * 3）
    segsize: usize,
}

// CUDA 句柄类型（driver API 指针）
type CUcontext = *mut c_void;
type CUmodule = *mut c_void;
type CUfunction = *mut c_void;
type CUdeviceptr = u64;

impl CudaKernels {
    /// 探测 CUDA：dlopen 驱动 + cuInit + device + context + module 装载。
    /// 任何一步失败返回 None（调用方回退 CPU）。
    pub fn probe() -> Option<CudaKernels> {
        let lib = unsafe { Library::new(CUDA_SONAME).ok()? };
        // 逐符号绑定，缺任一即放弃
        let cu_init: Symbol<unsafe extern "C" fn(u32) -> i32> = unsafe { lib.get(b"cuInit\0").ok()? };
        let cu_dev_get_count: Symbol<unsafe extern "C" fn(*mut i32) -> i32> =
            unsafe { lib.get(b"cuDeviceGetCount\0").ok()? };
        let cu_dev_get: Symbol<unsafe extern "C" fn(*mut i32, i32) -> i32> =
            unsafe { lib.get(b"cuDeviceGet\0").ok()? };
        let cu_ctx_create: Symbol<unsafe extern "C" fn(*mut CUcontext, u32, i32) -> i32> =
            unsafe { lib.get(b"cuCtxCreate_v2\0").ok()? };
        let cu_ctx_destroy: Symbol<unsafe extern "C" fn(CUcontext) -> i32> =
            unsafe { lib.get(b"cuCtxDestroy\0").ok()? };
        let cu_module_load: Symbol<unsafe extern "C" fn(*mut CUmodule, *const c_void, u32, *mut u32, *mut *mut c_void) -> i32> =
            unsafe { lib.get(b"cuModuleLoadDataEx\0").ok()? };
        let cu_module_func: Symbol<unsafe extern "C" fn(CUmodule, *mut CUfunction, *const libc::c_char) -> i32> =
            unsafe { lib.get(b"cuModuleGetFunction\0").ok()? };
        let cu_mem_alloc: Symbol<unsafe extern "C" fn(*mut CUdeviceptr, usize) -> i32> =
            unsafe { lib.get(b"cuMemAlloc_v2\0").ok()? };
        let _ = &cu_ctx_destroy;

        // 初始化
        if unsafe { cu_init(0) } != 0 {
            return None;
        }
        let mut ndev = 0i32;
        if unsafe { cu_dev_get_count(&mut ndev) } != 0 || ndev <= 0 {
            return None;
        }
        let mut dev = 0i32;
        if unsafe { cu_dev_get(&mut dev, 0) } != 0 {
            return None;
        }
        let mut ctx: CUcontext = std::ptr::null_mut();
        if unsafe { cu_ctx_create(&mut ctx, 0, dev) } != 0 {
            return None;
        }
        // 装载 PTX 模块
        let mut module: CUmodule = std::ptr::null_mut();
        let mut opts: [u32; 1] = [0];
        let mut optvals: [*mut c_void; 1] = [std::ptr::null_mut()];
        if unsafe { cu_module_load(&mut module, PTX.as_ptr() as *const c_void, opts[0], opts.as_mut_ptr(), optvals.as_mut_ptr()) } != 0 {
            let _ = unsafe { cu_ctx_destroy(ctx) };
            return None;
        }
        let getf = |name: &str| -> Option<CUfunction> {
            let cname = std::ffi::CString::new(name).ok()?;
            let mut f: CUfunction = std::ptr::null_mut();
            if unsafe { cu_module_func(module, &mut f, cname.as_ptr()) } != 0 {
                return None;
            }
            Some(f)
        };
        let f_filter = getf("filter_kernel")?;
        let f_dilate = getf("dilate_kernel")?;
        let f_diff = getf("diff_kernel")?;
        let f_scale = getf("scale_kernel")?;

        // 段参数 device 缓冲（16 u32 × 3 段）
        let mut segbuf: CUdeviceptr = 0;
        if unsafe { cu_mem_alloc(&mut segbuf, 16 * 4 * 3) } != 0 {
            let _ = unsafe { cu_ctx_destroy(ctx) };
            return None;
        }

        Some(CudaKernels {
            _lib: lib, ctx, mod_: module, f_filter, f_dilate, f_diff, f_scale,
            segbuf, segsize: 16 * 4 * 3,
        })
    }

    /// 上传段参数（16 u32/段）。
    fn upload_segments(&self, cfg: &FilterConfig) -> bool {
        let mut seg: Vec<u32> = vec![0; 16 * 3];
        let segs: Vec<crate::config::ColorSegment> = crate::filter::active_segments_pub(cfg);
        for (i, s) in segs.iter().enumerate().take(3) {
            let base = i * 16;
            seg[base + 0] = s.enable_rgb as u32;
            seg[base + 1] = s.rgb.0 as u32;
            seg[base + 2] = s.rgb.1 as u32;
            seg[base + 3] = s.rgb.2 as u32;
            seg[base + 4] = (s.rgb_diff * s.rgb_diff) as u32; // int32 平方距离
            seg[base + 5] = s.enable_hue as u32;
            seg[base + 6] = (s.hue % 180) as u32;
            seg[base + 7] = s.hue_diff as u32;
            seg[base + 8] = s.enable_lum_min as u32;
            seg[base + 9] = s.lum_min as u32;
            seg[base + 10] = s.enable_lum_max as u32;
            seg[base + 11] = s.lum_max as u32;
            seg[base + 12] = s.enable_sat_min as u32;
            seg[base + 13] = s.sat_min as u32;
            seg[base + 14] = s.enable_sat_max as u32;
            seg[base + 15] = s.sat_max as u32;
        }
        match unsafe { self._lib.get::<unsafe extern "C" fn(CUdeviceptr, *const c_void, usize) -> i32>(b"cuMemcpyHtoD_v2\0") } {
            Ok(f) => (unsafe { f(self.segbuf, seg.as_ptr() as *const c_void, seg.len() * 4) }) == 0,
            Err(_) => false,
        }
    }

    fn launch(&self, f: CUfunction, n: usize, block: u32, args: &[CUdeviceptr]) -> bool {
        let bx = (n as u32).div_ceil(block);
        let mut params: Vec<*mut c_void> = args.iter().map(|a| (*a) as usize as *mut c_void).collect();
        match unsafe { self._lib.get::<unsafe extern "C" fn(
                CUfunction, u32, u32, u32, u32, u32, u32, u32, *mut CUdeviceptr, *mut *mut c_void) -> i32>(b"cuLaunchKernel\0") } {
            Ok(launch) => unsafe {
                launch(f, bx, 1, 1, block, 1, 1, 0, std::ptr::null_mut(), params.as_mut_ptr()) == 0
            },
            Err(_) => false,
        }
    }

    fn ctx_sync(&self) -> bool {
        match unsafe { self._lib.get::<unsafe extern "C" fn(CUcontext) -> i32>(b"cuCtxSynchronize\0") } {
            Ok(f) => (unsafe { f(self.ctx) }) == 0,
            Err(_) => false,
        }
    }
}

// CUDA 指针句柄：ctx 创建后单线程使用（调用方在 rip 线程内独占），跨线程只传递 trait 对象。
unsafe impl Send for CudaKernels {}
unsafe impl Sync for CudaKernels {}

impl Drop for CudaKernels {
    fn drop(&mut self) {
        unsafe {
            if let Ok(f) = self._lib.get::<unsafe extern "C" fn(CUdeviceptr) -> i32>(b"cuMemFree_v2\0") {
                f(self.segbuf);
            }
            if let Ok(f) = self._lib.get::<unsafe extern "C" fn(CUmodule) -> i32>(b"cuModuleUnload\0") {
                f(self.mod_);
            }
            if let Ok(f) = self._lib.get::<unsafe extern "C" fn(CUcontext) -> i32>(b"cuCtxDestroy\0") {
                f(self.ctx);
            }
        }
    }
}

const CUDA_SONAME: &str = if cfg!(target_os = "windows") {
    "nvcuda.dll"
} else {
    "libcuda.so.1"
};

impl FrameKernels for CudaKernels {
    fn filter(&self, rgb: &[u8], w: usize, h: usize, cfg: &FilterConfig) -> Vec<u8> {
        let n = w * h;
        if n == 0 || !self.upload_segments(cfg) {
            return crate::filter::filter_frame_cpu(rgb, w, h, cfg);
        }
        unsafe {
            let alloc = match self._lib.get::<unsafe extern "C" fn(*mut CUdeviceptr, usize) -> i32>(b"cuMemAlloc_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::filter_frame_cpu(rgb, w, h, cfg),
            };
            let h2d = match self._lib.get::<unsafe extern "C" fn(CUdeviceptr, *const c_void, usize) -> i32>(b"cuMemcpyHtoD_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::filter_frame_cpu(rgb, w, h, cfg),
            };
            let d2h = match self._lib.get::<unsafe extern "C" fn(*mut c_void, CUdeviceptr, usize) -> i32>(b"cuMemcpyDtoH_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::filter_frame_cpu(rgb, w, h, cfg),
            };
            let free = match self._lib.get::<unsafe extern "C" fn(CUdeviceptr) -> i32>(b"cuMemFree_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::filter_frame_cpu(rgb, w, h, cfg),
            };

            let mut drgb: CUdeviceptr = 0;
            let mut dmask: CUdeviceptr = 0;
            let mut dmask2: CUdeviceptr = 0;
            if alloc(&mut drgb, rgb.len()) != 0 || alloc(&mut dmask, n) != 0 || alloc(&mut dmask2, n) != 0 {
                let _ = free(drgb); let _ = free(dmask); let _ = free(dmask2);
                return crate::filter::filter_frame_cpu(rgb, w, h, cfg);
            }
            if h2d(drgb, rgb.as_ptr() as *const c_void, rgb.len()) != 0 {
                let _ = free(drgb); let _ = free(dmask); let _ = free(dmask2);
                return crate::filter::filter_frame_cpu(rgb, w, h, cfg);
            }
            let ok = self.launch(self.f_filter, n, 256, &[drgb, dmask, self.segbuf, w as u64, h as u64, n as u64])
                && self.ctx_sync();
            let mut cur = if ok { dmask } else { dmask2 };
            // 膨胀 n 次（pixel_compensate）
            let iters = cfg.pixel_compensate.max(0);
            let mut a = dmask;
            let mut b = dmask2;
            for _i in 0..iters {
                let ok = self.launch(self.f_dilate, n, 256, &[a, b, w as u64, h as u64, n as u64])
                    && self.ctx_sync();
                if !ok {
                    cur = a;
                    break;
                }
                std::mem::swap(&mut a, &mut b);
                cur = a;
            }
            let mut out = vec![0u8; n];
            if d2h(out.as_mut_ptr() as *mut c_void, cur, n) != 0 {
                out = crate::filter::filter_frame_cpu(rgb, w, h, cfg);
            }
            let _ = free(drgb); let _ = free(dmask); let _ = free(dmask2);
            out
        }
    }

    fn frame_diff(&self, prev: &[u8], cur: &[u8], threshold: i64) -> (i64, f64, Vec<bool>) {
        let n = prev.len() / 3;
        if n == 0 {
            return (0, 0.0, vec![]);
        }
        unsafe {
            let alloc = match self._lib.get::<unsafe extern "C" fn(*mut CUdeviceptr, usize) -> i32>(b"cuMemAlloc_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::frame_diff_cpu(prev, cur, threshold),
            };
            let h2d = match self._lib.get::<unsafe extern "C" fn(CUdeviceptr, *const c_void, usize) -> i32>(b"cuMemcpyHtoD_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::frame_diff_cpu(prev, cur, threshold),
            };
            let d2h = match self._lib.get::<unsafe extern "C" fn(*mut c_void, CUdeviceptr, usize) -> i32>(b"cuMemcpyDtoH_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::frame_diff_cpu(prev, cur, threshold),
            };
            let free = match self._lib.get::<unsafe extern "C" fn(CUdeviceptr) -> i32>(b"cuMemFree_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::frame_diff_cpu(prev, cur, threshold),
            };

            let mut dp: CUdeviceptr = 0;
            let mut dc: CUdeviceptr = 0;
            let mut dout: CUdeviceptr = 0;
            let mut dcount: CUdeviceptr = 0;
            if alloc(&mut dp, prev.len()) != 0 || alloc(&mut dc, cur.len()) != 0
                || alloc(&mut dout, n) != 0 || alloc(&mut dcount, 4) != 0 {
                let _ = free(dp); let _ = free(dc); let _ = free(dout); let _ = free(dcount);
                return crate::filter::frame_diff_cpu(prev, cur, threshold);
            }
            if h2d(dp, prev.as_ptr() as *const c_void, prev.len()) != 0
                || h2d(dc, cur.as_ptr() as *const c_void, cur.len()) != 0 {
                let _ = free(dp); let _ = free(dc); let _ = free(dout); let _ = free(dcount);
                return crate::filter::frame_diff_cpu(prev, cur, threshold);
            }
            // 计数清零
            let zero = [0u32];
            if h2d(dcount, zero.as_ptr() as *const c_void, 4) != 0 {
                let _ = free(dp); let _ = free(dc); let _ = free(dout); let _ = free(dcount);
                return crate::filter::frame_diff_cpu(prev, cur, threshold);
            }
            let ok = self.launch(self.f_diff, n, 256,
                    &[dp, dc, dout, dcount, n as u64, threshold as u64])
                && self.ctx_sync();
            let mut out_u8 = vec![0u8; n];
            let mut cnt = [0u32];
            let ok = ok
                && d2h(out_u8.as_mut_ptr() as *mut c_void, dout, n) == 0
                && d2h(cnt.as_mut_ptr() as *mut c_void, dcount, 4) == 0;
            let _ = free(dp); let _ = free(dc); let _ = free(dout); let _ = free(dcount);
            if !ok {
                return crate::filter::frame_diff_cpu(prev, cur, threshold);
            }
            let changed = cnt[0] as i64;
            let ratio = if n > 0 { changed as f64 / n as f64 } else { 0.0 };
            let mask: Vec<bool> = out_u8.iter().map(|v| *v > 0).collect();
            (changed, ratio, mask)
        }
    }

    fn scale(&self, rgb: &[u8], w: usize, h: usize, factor: f64) -> (Vec<u8>, usize, usize) {
        if factor <= 0.0 {
            return crate::filter::scale_nearest_cpu(rgb, w, h, factor);
        }
        let (nw, nh) = ((w as f64 * factor) as usize, (h as f64 * factor) as usize);
        if nw == 0 || nh == 0 || (nw == w && nh == h) {
            return crate::filter::scale_nearest_cpu(rgb, w, h, factor);
        }
        let n = nw * nh;
        unsafe {
            let alloc = match self._lib.get::<unsafe extern "C" fn(*mut CUdeviceptr, usize) -> i32>(b"cuMemAlloc_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::scale_nearest_cpu(rgb, w, h, factor),
            };
            let h2d = match self._lib.get::<unsafe extern "C" fn(CUdeviceptr, *const c_void, usize) -> i32>(b"cuMemcpyHtoD_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::scale_nearest_cpu(rgb, w, h, factor),
            };
            let d2h = match self._lib.get::<unsafe extern "C" fn(*mut c_void, CUdeviceptr, usize) -> i32>(b"cuMemcpyDtoH_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::scale_nearest_cpu(rgb, w, h, factor),
            };
            let free = match self._lib.get::<unsafe extern "C" fn(CUdeviceptr) -> i32>(b"cuMemFree_v2\0") {
                Ok(f) => f, Err(_) => return crate::filter::scale_nearest_cpu(rgb, w, h, factor),
            };

            let mut ds: CUdeviceptr = 0;
            let mut dd: CUdeviceptr = 0;
            if alloc(&mut ds, rgb.len()) != 0 || alloc(&mut dd, n * 3) != 0 {
                let _ = free(ds); let _ = free(dd);
                return crate::filter::scale_nearest_cpu(rgb, w, h, factor);
            }
            if h2d(ds, rgb.as_ptr() as *const c_void, rgb.len()) != 0 {
                let _ = free(ds); let _ = free(dd);
                return crate::filter::scale_nearest_cpu(rgb, w, h, factor);
            }
            let ok = self.launch(self.f_scale, n, 256,
                    &[ds, dd, w as u64, h as u64, nw as u64, nh as u64])
                && self.ctx_sync();
            let mut out = vec![0u8; n * 3];
            let ok = ok && d2h(out.as_mut_ptr() as *mut c_void, dd, out.len()) == 0;
            let _ = free(ds); let _ = free(dd);
            if !ok {
                return crate::filter::scale_nearest_cpu(rgb, w, h, factor);
            }
            (out, nw, nh)
        }
    }
}

// ================================================================ PTX 内核
// sm_50 兼容（GeForce 900 系起），Win11 所有 NVIDIA 驱动可加载。
// 语义与 CPU 实现逐位一致：整数 HSV、int32 平方距离。
const PTX: &str = r#"
.version 6.0
.target sm_50
.address_size 64

// ---------- 整数 RGB->HSV：H 0..359, S/V 0..255 ----------
// h = (max-min)==0 ? 0 : 按最大通道取 60° 分段（整除）
// s = max==0 ? 0 : d*255/max ; v = max

// ---------- diff_kernel ----------
.visible .entry diff_kernel(
    .param .u64 prev, .param .u64 cur, .param .u64 outm, .param .u64 counter,
    .param .u32 n, .param .u32 thr)
{
    .reg .u32 tid, d0, d1, d2, md;
    .reg .u64 p, c, o, cb;
    .reg .u8 r1, g1, b1, r2, g2, b2;
    .reg .pred hit;
    cvta.to.global.u64 p, [prev];
    cvta.to.global.u64 c, [cur];
    cvta.to.global.u64 o, [outm];
    cvta.to.global.u64 cb, [counter];
    mov.u32 tid, %ctaid.x;
    mul.lo.u32 tid, tid, %ntid.x;
    add.u32 tid, tid, %tid.x;
    setp.ge.u32 hit, tid, n;
    @hit bra RET;
    // 读 6 字节
    mul.wide.u32 p, tid, 3;
    mul.wide.u32 c, tid, 3;
    ld.global.u8 r1, [p + p];
    ld.global.u8 g1, [p + p + 1];
    ld.global.u8 b1, [p + p + 2];
    ld.global.u8 r2, [c + c];
    ld.global.u8 g2, [c + c + 1];
    ld.global.u8 b2, [c + c + 2];
    abs.diff.u32 d0, r1, r2;
    abs.diff.u32 d1, g1, g2;
    abs.diff.u32 d2, b1, b2;
    max.u32 md, d0, d1;
    max.u32 md, md, d2;
    setp.gt.u32 hit, md, thr;
    st.global.u8 [o + tid], hit ? 1 : 0;
    @hit {
        atom.global.add.u32 [cb], 1;
    }
RET:
    ret;
}

// ---------- filter_kernel ----------
// segbase: u32[48]，每段 16：0=enable_rgb 1..3=rgb 4=diff2 5=enable_hue
// 6=hue180 7=hue_diff 8..11=lum_min/max+en 12..15=sat_min/max+en
.visible .entry filter_kernel(
    .param .u64 rgb, .param .u64 mask, .param .u64 segbase,
    .param .u32 w, .param .u32 h, .param .u32 n)
{
    .reg .u32 tid, r, g, b, mx, mn, d, hsv_s, hsv_v, hue, hue180;
    .reg .u32 en, rr, gg, bb, d2, e, v, s, base, dd, dr, dg, db;
    .reg .u64 p, o, sb;
    .reg .u8 r8, g8, b8;
    .reg .pred hit, ok;
    cvta.to.global.u64 p, [rgb];
    cvta.to.global.u64 o, [mask];
    cvta.to.global.u64 sb, [segbase];
    mov.u32 tid, %ctaid.x;
    mul.lo.u32 tid, tid, %ntid.x;
    add.u32 tid, tid, %tid.x;
    setp.ge.u32 hit, tid, n;
    @hit bra RET;
    mul.wide.u32 p, tid, 3;
    ld.global.u8 r8, [p + p];
    ld.global.u8 g8, [p + p + 1];
    ld.global.u8 b8, [p + p + 2];
    mov.u32 r, r8;
    mov.u32 g, g8;
    mov.u32 b, b8;
    // max/min
    max.u32 mx, r, g;
    max.u32 mx, mx, b;
    min.u32 mn, r, g;
    min.u32 mn, mn, b;
    sub.u32 d, mx, mn;
    mov.u32 hue, 0;
    setp.eq.u32 ok, d, 0;
    @!ok {
        // r==mx: h=((g-b)*60)/d ; g==mx: +120 ; b==mx: +240
        setp.eq.u32 ok, r, mx;
        @ok {
            sub.u32 dd, g, b;
            mad.lo.s32 hue, dd, 60, 0;
            div.s32 hue, hue, d;
        }
        @!ok {
            setp.eq.u32 ok, g, mx;
            @ok {
                sub.u32 dd, b, r;
                mad.lo.s32 hue, dd, 60, 120;
                div.s32 hue, hue, d;
            }
            @!ok {
                sub.u32 dd, r, g;
                mad.lo.s32 hue, dd, 60, 240;
                div.s32 hue, hue, d;
            }
        }
    }
    setp.lt.u32 ok, hue, 0;
    @ok add.u32 hue, hue, 360;
    div.u32 hue180, hue, 2;
    // s
    setp.eq.u32 ok, mx, 0;
    @!ok {
        mul.lo.u32 s, d, 255;
        div.u32 s, s, mx;
    }
    @ok mov.u32 s, 0;
    mov.u32 hsv_v, mx;
    // 逐段判据
    mov.u32 base, 0;
SEGLOOP:
    setp.ge.u32 ok, base, 48;
    @ok bra DONE;
    // enable_rgb
    ld.global.u32 en, [sb + base*4 + 0];
    setp.eq.u32 ok, en, 0;
    @!ok {
        ld.global.u32 rr, [sb + base*4 + 4];
        ld.global.u32 gg, [sb + base*4 + 8];
        ld.global.u32 bb, [sb + base*4 + 12];
        ld.global.u32 d2, [sb + base*4 + 16];
        sub.u32 dr, r, rr;
        sub.u32 dg, g, gg;
        sub.u32 db, b, bb;
        mad.lo.s32 dr, dr, dr, 0;
        mad.lo.s32 dg, dg, dg, 0;
        mad.lo.s32 db, db, db, 0;
        add.u32 dd, dr, dg;
        add.u32 dd, dd, db;
        setp.le.u32 ok, dd, d2;
        @!ok bra SEGNEXT;
    }
    // enable_hue
    ld.global.u32 e, [sb + base*4 + 20];
    setp.eq.u32 ok, e, 0;
    @!ok {
        ld.global.u32 v, [sb + base*4 + 24];
        ld.global.u32 d, [sb + base*4 + 28];
        abs.diff.u32 dr, hue180, v;
        mov.u32 dg, 180;
        sub.u32 dg, dg, dr;
        min.u32 dd, dr, dg;
        setp.le.u32 ok, dd, d;
        @!ok bra SEGNEXT;
    }
    // enable_lum_min
    ld.global.u32 e, [sb + base*4 + 32];
    setp.eq.u32 ok, e, 0;
    @!ok {
        ld.global.u32 v, [sb + base*4 + 36];
        setp.ge.u32 ok, hsv_v, v;
        @!ok bra SEGNEXT;
    }
    // enable_lum_max
    ld.global.u32 e, [sb + base*4 + 40];
    setp.eq.u32 ok, e, 0;
    @!ok {
        ld.global.u32 v, [sb + base*4 + 44];
        setp.le.u32 ok, hsv_v, v;
        @!ok bra SEGNEXT;
    }
    // enable_sat_min
    ld.global.u32 e, [sb + base*4 + 48];
    setp.eq.u32 ok, e, 0;
    @!ok {
        ld.global.u32 v, [sb + base*4 + 52];
        setp.ge.u32 ok, s, v;
        @!ok bra SEGNEXT;
    }
    // enable_sat_max
    ld.global.u32 e, [sb + base*4 + 56];
    setp.eq.u32 ok, e, 0;
    @!ok {
        ld.global.u32 v, [sb + base*4 + 60];
        setp.le.u32 ok, s, v;
        @!ok bra SEGNEXT;
    }
    st.global.u8 [o + tid], 1;
    bra RET;
SEGNEXT:
    add.u32 base, base, 64;
    bra SEGLOOP;
DONE:
    st.global.u8 [o + tid], 0;
RET:
    ret;
}

// ---------- dilate_kernel ----------
.visible .entry dilate_kernel(
    .param .u64 in, .param .u64 out, .param .u32 w, .param .u32 h, .param .u32 n)
{
    .reg .u32 tid, x, y, x0, x1, y0, y1, i, j, idx, v;
    .reg .u64 p, o;
    .reg .pred hit;
    cvta.to.global.u64 p, [in];
    cvta.to.global.u64 o, [out];
    mov.u32 tid, %ctaid.x;
    mul.lo.u32 tid, tid, %ntid.x;
    add.u32 tid, tid, %tid.x;
    setp.ge.u32 hit, tid, n;
    @hit bra RET;
    div.u32 y, tid, w;
    rem.u32 x, tid, w;
    mov.u32 x0, x;
    sub.u32 x0, x0, 1;
    setp.lt.u32 hit, x0, 0;
    @hit mov.u32 x0, 0;
    add.u32 x1, x, 1;
    setp.ge.u32 hit, x1, w;
    @hit sub.u32 x1, x1, 1;
    mov.u32 y0, y;
    sub.u32 y0, y0, 1;
    setp.lt.u32 hit, y0, 0;
    @hit mov.u32 y0, 0;
    add.u32 y1, y, 1;
    setp.ge.u32 hit, y1, h;
    @hit sub.u32 y1, y1, 1;
    mov.u32 v, 0;
    mov.u32 i, y0;
DL:
    setp.gt.u32 hit, i, y1;
    @hit bra DLEND;
    mov.u32 j, x0;
    DL2:
    setp.gt.u32 hit, j, x1;
    @hit bra DL2END;
    mad.lo.u32 idx, i, w, j;
    ld.global.u8 v2, [p + idx];
    setp.ne.u32 hit, v2, 0;
    @hit mov.u32 v, 255;
    add.u32 j, j, 1;
    bra DL2;
DL2END:
    add.u32 i, i, 1;
    bra DL;
DLEND:
    st.global.u8 [o + tid], v;
RET:
    ret;
}

// ---------- scale_kernel（最近邻） ----------
.visible .entry scale_kernel(
    .param .u64 src, .param .u64 dst, .param .u32 sw, .param .u32 sh,
    .param .u32 dw, .param .u32 dh)
{
    .reg .u32 tid, x, y, sx, sy, si, di;
    .reg .u64 p, o;
    .reg .u8 c0, c1, c2;
    .reg .pred hit;
    cvta.to.global.u64 p, [src];
    cvta.to.global.u64 o, [dst];
    mov.u32 tid, %ctaid.x;
    mul.lo.u32 tid, tid, %ntid.x;
    add.u32 tid, tid, %tid.x;
    mul.lo.u32 n, dh, dw;
    setp.ge.u32 hit, tid, n;
    @hit bra RET;
    div.u32 y, tid, dw;
    rem.u32 x, tid, dw;
    // sy = y*sh/dh ; sx = x*sw/dw
    mul.lo.u32 sy, y, sh;
    div.u32 sy, sy, dh;
    mul.lo.u32 sx, x, sw;
    div.u32 sx, sx, dw;
    mad.lo.u32 si, sy, sw, sx;
    mad.lo.u32 di, tid, 3, 0;
    mul.wide.u32 si, si, 3;
    ld.global.u8 c0, [p + si];
    ld.global.u8 c1, [p + si + 1];
    ld.global.u8 c2, [p + si + 2];
    st.global.u8 [o + di], c0;
    st.global.u8 [o + di + 1], c1;
    st.global.u8 [o + di + 2], c2;
RET:
    ret;
}
"#;
