//! 视频解码层 —— FFmpeg（ffmpeg-next 绑定）替代 esrXP 的 DirectShow 管线。
//!
//! 提供：元数据探测、流式顺序解码（seek 到关键帧后前向解码，回调式避免全量载入内存）、
//! RGB24 帧输出。解码后端：优先硬件（CUDA NVDEC / D3D11VA / VAAPI，运行时探测），
//! 失败自动回退软件解码（ffmpeg-next send_packet/receive_frame 流水线）。

use anyhow::Result;
use ffmpeg_next::ffi as ffi;
use ffmpeg_next::format::context::Input;
use ffmpeg_next::media::Type;
use ffmpeg_next::software::scaling::{Context as ScaleCtx, Flags};
use ffmpeg_next::util::format::Pixel;
use ffmpeg_next::util::frame::video::Video;
use ffmpeg_next::{format, frame, Error};

pub struct FrameData {
    pub index: i64,
    pub time: f64,
    pub rgb: Vec<u8>,          // packed RGB24, w*h*3
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecodeBackend {
    Cuda,
    D3D11Va,
    Vaapi,
    Cpu,
}

impl DecodeBackend {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cuda => "CUDA (NVDEC)",
            Self::D3D11Va => "D3D11VA",
            Self::Vaapi => "VAAPI",
            Self::Cpu => "CPU (软件)",
        }
    }
    fn hw_type(&self) -> u32 {
        use ffi::AVHWDeviceType::*;
        match self {
            Self::Cuda => AV_HWDEVICE_TYPE_CUDA as u32,
            Self::D3D11Va => AV_HWDEVICE_TYPE_D3D11VA as u32,
            Self::Vaapi => AV_HWDEVICE_TYPE_VAAPI as u32,
            Self::Cpu => 0,
        }
    }

    fn hw_ffi_type(&self) -> ffi::AVHWDeviceType {
        let v = self.hw_type();
        unsafe { std::mem::transmute::<u32, ffi::AVHWDeviceType>(v) }
    }

    fn hw_pix_fmt(&self) -> ffi::AVPixelFormat {
        use ffi::AVPixelFormat::*;
        let v = match self {
            Self::Cuda => AV_PIX_FMT_CUDA as u32,
            Self::D3D11Va => AV_PIX_FMT_D3D11VA_VLD as u32,
            // AV_PIX_FMT_NONE：让 av_hwframe_ctx_init 自动选择 VAAPI 输出格式，
            // 兼容 FFmpeg 5.0（vaapi_vld）与 5.1+/8.x/9.x（vaapi）枚举名差异
            Self::Vaapi => AV_PIX_FMT_NONE as u32,
            Self::Cpu => AV_PIX_FMT_YUV420P as u32,
        };
        unsafe { std::mem::transmute::<u32, ffi::AVPixelFormat>(v) }
    }
}

pub struct VideoSource {
    pub path: String,
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    pub duration: f64,
    pub frame_count: i64,
    pub time_base: f64,
    pub codec_name: String,
    pub pix_fmt: String,
    pub decode_backend: DecodeBackend,
    input: Input,
    stream_index: usize,
}

impl VideoSource {
    pub fn open(path: &str) -> Result<Self> {
        ffmpeg_next::init()?;
        let input = format::input(&path)?;
        let stream = input
            .streams()
            .best(Type::Video)
            .ok_or_else(|| anyhow::anyhow!("没有视频流"))?;
        let index = stream.index();
        // 元数据：直接读 codec context ffi 字段（Parameters 薄封装无宽高）
        let par = stream.parameters();
        let (width, height, pix_fmt_id) = unsafe {
            let p = par.as_ptr();
            ((*p).width as usize, (*p).height as usize, (*p).format as i32)
        };
        let tb = stream.time_base();
        let time_base = if tb.denominator() > 0 {
            tb.numerator() as f64 / tb.denominator() as f64
        } else {
            0.0
        };
        let afr = stream.avg_frame_rate();
        let fps = if afr.denominator() > 0 {
            afr.numerator() as f64 / afr.denominator() as f64
        } else {
            0.0
        };
        let fps = if (1.0..=1000.0).contains(&fps) { fps } else { 25.0 };
        let dur_us = input.duration();
        let dur_stream = stream.duration() as f64 * time_base;
        let duration = if dur_us > 0 && (dur_us as f64 / 1e6) < 1e5 {
            dur_us as f64 / 1e6
        } else if (0.0..1e5).contains(&dur_stream) {
            dur_stream
        } else {
            0.0
        };
        let frame_count = if stream.frames() > 0 {
            stream.frames()
        } else if duration > 0.0 {
            (duration * fps).round() as i64
        } else {
            0
        };
        let codec_name = par.id().name().to_string();
        let pix_enum: ffi::AVPixelFormat = unsafe { std::mem::transmute::<i32, ffi::AVPixelFormat>(pix_fmt_id) };
        let pix_fmt = Pixel::from(pix_enum)
            .descriptor()
            .map(|d| d.name().to_string())
            .unwrap_or_default();
        // 0.4.2：hw 解码链（NVDEC/D3D11VA）在 Windows GNU 构建 + FFmpeg 共享 DLL 组合下
        // 存在堆损坏（0xC0000374，preview/rip 实测必崩；Linux CPU 路径从不复现）。
        // 本工具为离线处理场景，CPU 软解足够，默认回退 CPU；
        // 设 ESRXP_HWDEC=1 可强制启用 hw 探测，仅用于后续排查。
        let hwdec_env = std::env::var("ESRXP_HWDEC").map(|v| v == "1").unwrap_or(false);
        let backend = if hwdec_env { probe_hw() } else { DecodeBackend::Cpu };
        Ok(Self {
            path: path.to_string(),
            width,
            height,
            fps,
            duration,
            frame_count,
            time_base,
            codec_name,
            pix_fmt,
            decode_backend: backend,
            input,
            stream_index: index,
        })
    }

    /// seek 到目标秒附近关键帧（av_seek_frame, BACKWARD）。
    pub fn seek_seconds(&mut self, seconds: f64) -> Result<()> {
        let stream = self.input.streams().best(Type::Video)
            .ok_or_else(|| anyhow::anyhow!("no video stream"))?;
        let tb = stream.time_base();
        let den = if tb.denominator() > 0 { tb.denominator() as i64 } else { 1 };
        let ts = (seconds * den as f64).round() as i64;
        unsafe {
            ffi::av_seek_frame(
                self.input.as_mut_ptr(),
                self.stream_index as i32,
                ts,
                ffi::AVSEEK_FLAG_BACKWARD,
            );
        }
        Ok(())
    }

    /// 流式顺序解码 [start, end) 按 step 抽样，每帧回调一次。
    pub fn decode_range<F>(&mut self, start: i64, end: i64, step: i64, mut cb: F) -> Result<()>
    where
        F: FnMut(FrameData) -> Result<()>,
    {
        let start = start.max(0);
        let end = if end > self.frame_count && self.frame_count > 0 {
            self.frame_count
        } else {
            end
        };
        if end <= start || step < 1 {
            return Ok(());
        }
        // 硬件解码尝试（尽力而为）；失败自动重开软件解码
        let mut produced = false;
        if self.decode_backend != DecodeBackend::Cpu {
            let ok = self.try_decode_hw(start, end, step, &mut produced, &mut cb)?;
            if ok {
                return Ok(());
            }
            // 回退软件并重扫
            self.decode_backend = DecodeBackend::Cpu;
        }
        self.decode_sw(start, end, step, &mut produced, &mut cb)?;
        if !produced {
            // 兜底：seek 漂移，从头顺序扫
            self.seek_seconds(0.0)?;
            self.decode_sw(start, end, step, &mut produced, &mut cb)?;
        }
        Ok(())
    }

    fn decode_sw<F>(&mut self, start: i64, end: i64, step: i64,
                    produced: &mut bool, cb: &mut F) -> Result<()>
    where
        F: FnMut(FrameData) -> Result<()>,
    {
        self.seek_seconds(start as f64 / self.fps - 0.2)?;
        let stream_index = self.stream_index;
        let stream = self.input.streams().best(Type::Video)
            .ok_or_else(|| anyhow::anyhow!("no video"))?;
        let mut decoder = ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .video()?;
        let mut scaler = ScaleCtx::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            Pixel::RGB24,
            decoder.width(),
            decoder.height(),
            Flags::BILINEAR,
        )?;
        let mut counter: i64 = -1;
        let mut first_frame = true;
        let mut packets = self.input.packets();
        'outer: while let Some((s, packet)) = packets.next() {
            if s.index() != stream_index {
                continue;
            }
            decoder.send_packet(&packet)?;
            loop {
                let mut f = Video::empty();
                match decoder.receive_frame(&mut f) {
                    Ok(()) => {
                        counter += 1;
                        let mut idx = frame_index(&f, counter, self.time_base, self.fps);
                        // 首帧锚定：录屏等来源的首帧 pts 常有偏移（如 0.02s），
                        // pts 换算会把请求区间起点跳过（idx > start 且此前无任何产出），
                        // 造成 start=0 时「帧不存在」。将本段解码的第一帧锚定到区间起点。
                        if first_frame && idx > start {
                            idx = start;
                        }
                        first_frame = false;
                        if idx < start {
                            continue;
                        }
                        if idx >= end {
                            break 'outer;
                        }
                        if (idx - start) % step == 0 {
                            let mut rgb = Video::empty();
                            scaler.run(&f, &mut rgb)?;
                            let data = copy_rgb24(&rgb, self.width, self.height);
                            *produced = true;
                            cb(FrameData {
                                index: idx,
                                time: frame_seconds(&f, self.time_base, idx),
                                rgb: data,
                                width: self.width,
                                height: self.height,
                            })?;
                        }
                    }
                    Err(Error::Eof) | Err(Error::Other { errno: 11 }) => break,
                    Err(e) => return Err(anyhow::anyhow!("解码失败: {e}")),
                }
            }
        }
        Ok(())
    }

    /// 硬件解码（NVDEC/D3D11VA/VAAPI）：hw 帧接收后 transfer 回系统内存再走管线。
    /// 返回 Ok(true) 表示 hw 路径成功产出；Ok(false) 表示应回退软件。
    fn try_decode_hw<F>(&mut self, start: i64, end: i64, step: i64,
                        produced: &mut bool, cb: &mut F) -> Result<bool>
    where
        F: FnMut(FrameData) -> Result<()>,
    {
        let backend = self.decode_backend;
        let stream = self.input.streams().best(Type::Video)
            .ok_or_else(|| anyhow::anyhow!("no video"))?;
        // 创建硬件设备上下文
        let mut hw_ctx: *mut ffi::AVBufferRef = std::ptr::null_mut();
        let rc = unsafe {
            ffi::av_hwdevice_ctx_create(&mut hw_ctx, backend.hw_ffi_type(),
                                        std::ptr::null(), std::ptr::null_mut(), 0)
        };
        if rc < 0 || hw_ctx.is_null() {
            return Ok(false);
        }
        // 解码器（自定义 open，注入 hw_frames_ctx）
        let mut codec_ctx = unsafe { ffi::avcodec_alloc_context3(std::ptr::null()) };
        if codec_ctx.is_null() {
            unsafe { ffi::av_buffer_unref(&mut hw_ctx) };
            return Ok(false);
        }
        let par = stream.parameters();
        let rc = unsafe { ffi::avcodec_parameters_to_context(codec_ctx, par.as_ptr()) };
        if rc < 0 {
            unsafe {
                ffi::avcodec_free_context(&mut codec_ctx);
                ffi::av_buffer_unref(&mut hw_ctx);
            }
            return Ok(false);
        }
        // 分配 hw_frames_ctx（hwaccel 需要知道输出格式/尺寸）
        let mut hw_frames: *mut ffi::AVBufferRef = std::ptr::null_mut();
        unsafe {
            let mut ctx = ffi::av_hwframe_ctx_alloc(hw_ctx);
            if !ctx.is_null() {
                let hwfc = (*ctx).data as *mut ffi::AVHWFramesContext;
                (*hwfc).format = backend.hw_pix_fmt();
                (*hwfc).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
                (*hwfc).width = self.width as i32;
                (*hwfc).height = self.height as i32;
                let rc = ffi::av_hwframe_ctx_init(ctx);
                if rc < 0 {
                    ffi::av_buffer_unref(&mut ctx);
                } else {
                    hw_frames = ctx;
                }
            }
        }
        if hw_frames.is_null() {
            unsafe {
                ffi::avcodec_free_context(&mut codec_ctx);
                ffi::av_buffer_unref(&mut hw_ctx);
            }
            return Ok(false);
        }
        unsafe {
            (*codec_ctx).hw_frames_ctx = hw_frames;
        }
        // 所有权已移交 codec_ctx（由 avcodec_free_context 统一释放）；本地指针必须置空。
        // 否则清理路径 avcodec_free_context 释放 hw_frames_ctx 后，下方 av_buffer_unref
        // 会再次解引用已释放的 AVBufferRef（use-after-free -> 0xC0000374 堆损坏）
        let mut hw_frames: *mut ffi::AVBufferRef = std::ptr::null_mut();
        // 打开解码器
        let decoder_codec = unsafe { ffi::avcodec_find_decoder((*codec_ctx).codec_id) };
        if decoder_codec.is_null() {
            unsafe {
                ffi::avcodec_free_context(&mut codec_ctx);
                ffi::av_buffer_unref(&mut hw_ctx);
                if !hw_frames.is_null() {
                    ffi::av_buffer_unref(&mut hw_frames);
                }
            }
            return Ok(false);
        }
        let rc = unsafe { ffi::avcodec_open2(codec_ctx, decoder_codec, std::ptr::null_mut()) };
        if rc < 0 {
            unsafe {
                ffi::avcodec_free_context(&mut codec_ctx);
                ffi::av_buffer_unref(&mut hw_ctx);
                if !hw_frames.is_null() {
                    ffi::av_buffer_unref(&mut hw_frames);
                }
            }
            return Ok(false);
        }
        // 解码循环（hw 帧 → transfer → sw 帧 → swscale → RGB24）
        let mut result = Ok(true);
        let mut packets = self.input.packets();
        let mut counter: i64 = -1;
        let mut first_frame = true;
        'outer: while let Some((s, packet)) = packets.next() {
            if s.index() != self.stream_index {
                continue;
            }
            // 构造裸 AVPacket（data 指向迭代器包生命周期内）
            let mut pkt: ffi::AVPacket = unsafe { std::mem::zeroed() };
            if let Some(data) = packet.data() {
                pkt.data = data.as_ptr() as *mut u8;
                pkt.size = data.len() as i32;
            }
            pkt.pts = packet.pts().unwrap_or(ffi::AV_NOPTS_VALUE);
            pkt.dts = packet.dts().unwrap_or(ffi::AV_NOPTS_VALUE);
            pkt.flags = packet.flags().bits();
            let rc = unsafe { ffi::avcodec_send_packet(codec_ctx, &mut pkt) };
            if rc < 0 {
                continue;
            }
            loop {
                let mut hw_frame = unsafe { frame::Video::empty() };
                let rc = unsafe {
                    ffi::avcodec_receive_frame(codec_ctx, hw_frame.as_mut_ptr())
                };
                if rc == ffi::AVERROR_EOF as i32 || rc == -11 {
                    break;
                }
                if rc < 0 {
                    break;
                }
                counter += 1;
                let mut idx = frame_index(&hw_frame, counter, self.time_base, self.fps);
                // 首帧锚定，语义与 decode_sw 一致
                if first_frame && idx > start {
                    idx = start;
                }
                first_frame = false;
                if idx < start {
                    continue;
                }
                if idx >= end {
                    break 'outer;
                }
                if (idx - start) % step != 0 {
                    continue;
                }
                // 转回系统内存
                let mut sw = unsafe { frame::Video::empty() };
                let tr = unsafe { ffi::av_hwframe_transfer_data(sw.as_mut_ptr(), hw_frame.as_ptr(), 0) };
                if tr < 0 {
                    continue;
                }
                let mut scaler = match ScaleCtx::get(
                    sw.format(),
                    sw.width() as u32,
                    sw.height() as u32,
                    Pixel::RGB24,
                    self.width as u32,
                    self.height as u32,
                    Flags::BILINEAR,
                ) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut rgb = Video::empty();
                if scaler.run(&sw, &mut rgb).is_err() {
                    continue;
                }
                let data = copy_rgb24(&rgb, self.width, self.height);
                *produced = true;
                if cb(FrameData {
                    index: idx,
                    time: frame_seconds(&hw_frame, self.time_base, idx),
                    rgb: data,
                    width: self.width,
                    height: self.height,
                })
                .is_err()
                {
                    result = Err(anyhow::anyhow!("回调错误"));
                    break 'outer;
                }
            }
        }
        unsafe {
            ffi::avcodec_free_context(&mut codec_ctx);
            ffi::av_buffer_unref(&mut hw_ctx);
            if !hw_frames.is_null() {
                ffi::av_buffer_unref(&mut hw_frames);
            }
        }
        result
    }
}


/// 硬件解码探测：CUDA → D3D11VA(Win) → VAAPI(Linux)，均失败回 CPU。
pub fn probe_hw() -> DecodeBackend {
    let mut order: Vec<DecodeBackend> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        order.push(DecodeBackend::Cuda);
        order.push(DecodeBackend::D3D11Va);
    }
    #[cfg(not(target_os = "windows"))]
    {
        order.push(DecodeBackend::Cuda);
        order.push(DecodeBackend::Vaapi);
    }
    for b in order {
        let mut ctx: *mut ffi::AVBufferRef = std::ptr::null_mut();
        let rc = unsafe {
            ffi::av_hwdevice_ctx_create(&mut ctx, b.hw_ffi_type(),
                                        std::ptr::null(), std::ptr::null_mut(), 0)
        };
        if rc >= 0 && !ctx.is_null() {
            unsafe { ffi::av_buffer_unref(&mut ctx) };
            return b;
        }
    }
    DecodeBackend::Cpu
}

/// 帧序号：pts 换算（time_base→秒→帧号），pts 缺失回退计数器。
fn frame_index(frame: &frame::Video, fallback: i64, time_base: f64, fps: f64) -> i64 {
    if let Some(pts) = frame.pts() {
        let t = pts as f64 * time_base;
        return (t * fps).round() as i64;
    }
    fallback
}

fn frame_seconds(frame: &frame::Video, time_base: f64, fallback_idx: i64) -> f64 {
    if let Some(pts) = frame.pts() {
        pts as f64 * time_base
    } else {
        fallback_idx as f64 / fps_of(fallback_idx, time_base)
    }
}

fn fps_of(_idx: i64, _tb: f64) -> f64 {
    25.0
}

/// 从 swscale 输出的 packed RGB24 帧按行拷贝（行对齐可能 > w*3）。
fn copy_rgb24(frame: &frame::Video, width: usize, height: usize) -> Vec<u8> {
    let src = frame.data(0);
    let stride = frame.stride(0) as usize;
    let mut out = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        out.extend_from_slice(&src[row * stride..row * stride + width * 3]);
    }
    out
}
