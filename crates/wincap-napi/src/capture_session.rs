use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi::JsFunction;
use napi_derive::napi;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITORINFO};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Graphics::DirectX::DirectXPixelFormat;

use wincap_core::clock;
use wincap_core::d3d_device::D3DDevice;
use wincap_core::error::WincapError;
use wincap_core::frame_pool::FramePool;
use wincap_core::mf_encoder::{EncoderConfig, FrameOptions, MfEncoder, VideoCodec};
use wincap_core::video_processor::{ColorSpace, VideoProcessor};
use wincap_core::wgc_source::{WgcOptions, WgcSource};
use windows::Win32::Graphics::Direct3D11::*;

#[napi(object)]
pub struct CaptureOptionsJs {
    pub source: SourceSpec,
    pub delivery: Option<DeliverySpec>,
    pub fps: Option<u32>,
    pub include_cursor: Option<bool>,
    pub border_required: Option<bool>,
}

#[napi(object)]
pub struct SourceSpec {
    pub kind: String,
    pub monitor_handle: Option<BigInt>,
    pub hwnd: Option<BigInt>,
}

#[napi(object)]
pub struct DeliverySpec {
    #[napi(js_name = "type")]
    pub delivery_type: String,
    pub codec: Option<String>,
    pub bitrate_bps: Option<u32>,
    pub fps: Option<u32>,
    pub keyframe_interval_ms: Option<u32>,
    pub hdr10: Option<bool>,
    pub ltr_count: Option<u32>,
    pub intra_refresh: Option<bool>,
    pub intra_refresh_period: Option<u32>,
    pub roi_enabled: Option<bool>,
}

#[napi(object)]
pub struct CaptureStatsJs {
    pub delivered_frames: BigInt,
    pub dropped_frames: BigInt,
    pub encoded_units: BigInt,
}

// Internal payload types for TSFN
struct FramePayload {
    timestamp_ns: u64,
    width: u32,
    height: u32,
    format: &'static str,
    size_changed: bool,
    /// CPU-readback pixel data (only populated in Cpu delivery mode).
    data: Option<Vec<u8>>,
    /// Row stride in bytes (only populated in Cpu delivery mode).
    stride: u32,
}

struct EncodedPayload {
    data: Vec<u8>,
    timestamp_ns: u64,
    keyframe: bool,
}

struct ErrorPayload {
    component: String,
    hresult: i32,
    message: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeliveryMode {
    Raw,
    Cpu,
    Encoded,
}

struct CaptureInner {
    device: DummySend<Box<D3DDevice>>,
    pool: DummySend<Box<FramePool>>,
    source: Option<DummySend<WgcSource>>,
    delivery: DeliveryMode,
    enc_cfg: EncoderConfig,
    // Encoder pipeline (lazy-initialized on first frame for Encoded mode)
    color: Option<DummySend<VideoProcessor>>,
    encoder: Option<DummySend<MfEncoder>>,
    enc_width: u32,
    enc_height: u32,
    /// Set after encoder init fails to prevent retry spam every frame.
    encoder_failed: bool,
    // NV12/P010 texture used as intermediate for color conversion
    nv12_texture: Option<ID3D11Texture2D>,
    // CPU readback state
    staging_textures: Vec<ID3D11Texture2D>,
    staging_fences: Vec<ID3D11Query>,
    staging_w: u32,
    staging_h: u32,
    staging_idx: u32,
}

// D3DDevice, FramePool, WgcSource are Send via their unsafe impl Send
struct DummySend<T>(T);
unsafe impl<T> Send for DummySend<T> {}
unsafe impl<T> Sync for DummySend<T> {}
impl<T> std::ops::Deref for DummySend<T> {
    type Target = T;
    fn deref(&self) -> &T { &self.0 }
}
impl<T> std::ops::DerefMut for DummySend<T> {
    fn deref_mut(&mut self) -> &mut T { &mut self.0 }
}

#[napi]
pub struct CaptureSession {
    inner: parking_lot::Mutex<CaptureInner>,
    on_frame: ThreadsafeFunction<FramePayload, ErrorStrategy::Fatal>,
    on_encoded: ThreadsafeFunction<EncodedPayload, ErrorStrategy::Fatal>,
    on_error: ThreadsafeFunction<ErrorPayload, ErrorStrategy::Fatal>,
    running: AtomicBool,
    delivered_frames: AtomicU64,
    dropped_frames: AtomicU64,
    encoded_units: AtomicU64,
}

#[napi]
impl CaptureSession {
    #[napi(constructor)]
    pub fn new(
        _env: Env,
        opts: CaptureOptionsJs,
        on_frame: JsFunction,
        on_encoded: JsFunction,
        on_error: JsFunction,
    ) -> Result<Self> {
        clock::init();

        let on_frame_tsfn = on_frame.create_threadsafe_function(4, |ctx: ThreadSafeCallContext<FramePayload>| {
            let mut obj = ctx.env.create_object()?;
            obj.set("timestampNs", ctx.env.create_bigint_from_u64(ctx.value.timestamp_ns)?.into_unknown())?;
            obj.set("width", ctx.value.width)?;
            obj.set("height", ctx.value.height)?;
            obj.set("format", ctx.value.format)?;
            obj.set("sizeChanged", ctx.value.size_changed)?;
            obj.set("stride", ctx.value.stride)?;
            if let Some(data) = ctx.value.data {
                let buf = ctx.env.create_buffer_with_data(data)?;
                obj.set("data", buf.into_raw())?;
            }
            Ok(vec![obj])
        })?;

        let on_encoded_tsfn = on_encoded.create_threadsafe_function(16, |ctx: ThreadSafeCallContext<EncodedPayload>| {
            let mut obj = ctx.env.create_object()?;
            let buf = ctx.env.create_buffer_with_data(ctx.value.data)?;
            obj.set("data", buf.into_raw())?;
            obj.set("timestampNs", ctx.env.create_bigint_from_u64(ctx.value.timestamp_ns)?.into_unknown())?;
            obj.set("keyframe", ctx.value.keyframe)?;
            Ok(vec![obj])
        })?;

        let on_error_tsfn = on_error.create_threadsafe_function(8, |ctx: ThreadSafeCallContext<ErrorPayload>| {
            let mut obj = ctx.env.create_object()?;
            obj.set("component", ctx.value.component.as_str())?;
            obj.set("hresult", ctx.value.hresult)?;
            obj.set("message", ctx.value.message.as_str())?;
            Ok(vec![obj])
        })?;

        // Parse delivery mode.
        let mut delivery = DeliveryMode::Raw;
        let mut enc_cfg = EncoderConfig::default();

        if let Some(ref d) = opts.delivery {
            match d.delivery_type.as_str() {
                "encoded" => {
                    delivery = DeliveryMode::Encoded;
                    enc_cfg.codec = match d.codec.as_deref() {
                        Some("hevc") => VideoCodec::HEVC,
                        Some("av1") => VideoCodec::AV1,
                        _ => VideoCodec::H264,
                    };
                    enc_cfg.bitrate_bps = d.bitrate_bps.unwrap_or(6_000_000);
                    enc_cfg.fps = d.fps.unwrap_or(60);
                    enc_cfg.keyframe_interval_ms = d.keyframe_interval_ms.unwrap_or(2000);
                    enc_cfg.hdr10 = d.hdr10.unwrap_or(false);
                    enc_cfg.ltr_count = d.ltr_count.unwrap_or(0);
                    enc_cfg.intra_refresh = d.intra_refresh.unwrap_or(false);
                    enc_cfg.intra_refresh_period = d.intra_refresh_period.unwrap_or(60);
                    enc_cfg.roi_enabled = d.roi_enabled.unwrap_or(false);
                }
                "cpu" => delivery = DeliveryMode::Cpu,
                "raw" | _ => delivery = DeliveryMode::Raw,
            }
        }

        // Create D3D device and frame pool on the heap so WgcSource's
        // raw pointers remain valid after we move them into the Mutex.
        let device = Box::new(D3DDevice::create(LUID::default()).map_err(to_napi_err)?);

        let hdr_capture = delivery == DeliveryMode::Encoded && enc_cfg.hdr10;
        let format = if hdr_capture {
            DXGI_FORMAT_R16G16B16A16_FLOAT
        } else {
            DXGI_FORMAT_B8G8R8A8_UNORM
        };

        // Determine capture resolution from the source to allocate
        // properly sized pool textures.
        let (init_w, init_h) = match opts.source.kind.as_str() {
            "display" => {
                let handle = opts.source.monitor_handle.as_ref()
                    .ok_or_else(|| Error::from_reason("display source requires monitorHandle"))?;
                let (_, val, _) = handle.get_u64();
                let hmon = HMONITOR(val as *mut _);
                let mut mi = MONITORINFO::default();
                mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                if unsafe { windows::Win32::Graphics::Gdi::GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
                    let r = mi.rcMonitor;
                    ((r.right - r.left) as u32, (r.bottom - r.top) as u32)
                } else {
                    (1920, 1080) // fallback
                }
            }
            "window" => {
                let handle = opts.source.hwnd.as_ref()
                    .ok_or_else(|| Error::from_reason("window source requires hwnd"))?;
                let (_, val, _) = handle.get_u64();
                let hwnd = HWND(val as *mut _);
                let mut r = RECT::default();
                if unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut r) }.is_ok() {
                    (((r.right - r.left).max(1)) as u32, ((r.bottom - r.top).max(1)) as u32)
                } else {
                    (1920, 1080)
                }
            }
            _ => (1920, 1080),
        };

        let desc = D3D11_TEXTURE2D_DESC {
            Width: init_w,
            Height: init_h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };

        let mut pool = Box::new(FramePool::new());
        pool.init(&device.device, 4, desc, false).map_err(to_napi_err)?;

        let mut wgc_opts = WgcOptions::default();
        if let Some(v) = opts.include_cursor { wgc_opts.include_cursor = v; }
        if let Some(v) = opts.border_required { wgc_opts.border_required = v; }
        if hdr_capture {
            wgc_opts.pixel_format = DirectXPixelFormat::R16G16B16A16Float;
        }

        // WgcSource stores raw pointers to device/pool. Since both are Box-allocated,
        // their heap addresses are stable even after moving into the Mutex.
        let mut source = WgcSource::new(&device, &pool, wgc_opts);

        match opts.source.kind.as_str() {
            "display" => {
                let handle = opts.source.monitor_handle
                    .ok_or_else(|| Error::from_reason("display source requires monitorHandle"))?;
                let (_, val, _) = handle.get_u64();
                source.init_for_monitor(HMONITOR(val as *mut _)).map_err(to_napi_err)?;
            }
            "window" => {
                let handle = opts.source.hwnd
                    .ok_or_else(|| Error::from_reason("window source requires hwnd"))?;
                let (_, val, _) = handle.get_u64();
                source.init_for_window(HWND(val as *mut _)).map_err(to_napi_err)?;
            }
            _ => return Err(Error::from_reason("source.kind must be 'display' or 'window'")),
        }

        Ok(Self {
            inner: parking_lot::Mutex::new(CaptureInner {
                device: DummySend(device),
                pool: DummySend(pool),
                source: Some(DummySend(source)),
                delivery,
                enc_cfg,
                color: None,
                encoder: None,
                enc_width: 0,
                enc_height: 0,
                encoder_failed: false,
                nv12_texture: None,
                staging_textures: Vec::new(),
                staging_fences: Vec::new(),
                staging_w: 0,
                staging_h: 0,
                staging_idx: 0,
            }),
            on_frame: on_frame_tsfn,
            on_encoded: on_encoded_tsfn,
            on_error: on_error_tsfn,
            running: AtomicBool::new(false),
            delivered_frames: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            encoded_units: AtomicU64::new(0),
        })
    }

    #[napi]
    pub fn start(&self) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let mut inner = self.inner.lock();
        let delivery = inner.delivery;

        let source = inner.source.as_mut()
            .ok_or_else(|| Error::from_reason("capture source not initialized"))?;

        let on_error_cb = self.on_error.clone();

        let err_cb = Box::new(move |component: &'static str, hr: i32, msg: &str| {
            let _ = on_error_cb.call(
                ErrorPayload {
                    component: component.to_string(),
                    hresult: hr,
                    message: msg.to_string(),
                },
                ThreadsafeFunctionCallMode::NonBlocking,
            );
        });
        let on_frame_clone = self.on_frame.clone();
        let on_encoded_clone = self.on_encoded.clone();
        let on_error_clone = self.on_error.clone();

        // Wrap raw pointers in a Send+Sync newtype so the closure can be Send+Sync.
        // SAFETY: CaptureSession (and thus these atomics + mutex) outlive the WgcSource
        // because we call source.stop() in stop()/drop before dropping inner.
        #[derive(Clone, Copy)]
        struct RawSend(*const ());
        unsafe impl Send for RawSend {}
        unsafe impl Sync for RawSend {}
        impl RawSend {
            fn ptr(self) -> *const () { self.0 }
        }

        let inner_ptr = RawSend(&self.inner as *const parking_lot::Mutex<CaptureInner> as *const ());
        let delivered_ptr = RawSend(&self.delivered_frames as *const AtomicU64 as *const ());
        let encoded_ptr = RawSend(&self.encoded_units as *const AtomicU64 as *const ());

        let frame_cb = Box::new(move |frame: wincap_core::wgc_source::CapturedFrame<'_>| {
            let inner_mutex = unsafe { &*(inner_ptr.ptr() as *const parking_lot::Mutex<CaptureInner>) };
            let delivered = unsafe { &*(delivered_ptr.ptr() as *const AtomicU64) };
            let encoded_units = unsafe { &*(encoded_ptr.ptr() as *const AtomicU64) };

            match delivery {
                DeliveryMode::Raw => {
                    // Raw mode: send metadata only, auto-release slot immediately.
                    let _ = on_frame_clone.call(
                        FramePayload {
                            timestamp_ns: frame.timestamp_ns,
                            width: frame.width,
                            height: frame.height,
                            format: "bgra8",
                            size_changed: frame.size_changed,
                            data: None,
                            stride: 0,
                        },
                        ThreadsafeFunctionCallMode::NonBlocking,
                    );
                    delivered.fetch_add(1, Ordering::Relaxed);
                    // Slot is auto-released when CapturedFrame drops (via SlotGuard in wgc_source),
                    // but wgc_source disarms the guard and passes ownership to us.
                    // We must release the slot manually.
                    let guard = inner_mutex.lock();
                    guard.pool.release(frame.slot);
                }
                DeliveryMode::Cpu => {
                    // CPU readback: copy to staging texture, map, send bytes.
                    let mut guard = inner_mutex.lock();
                    let w = frame.width;
                    let h = frame.height;

                    // Recreate staging textures on size change.
                    if w != guard.staging_w || h != guard.staging_h {
                        guard.staging_textures.clear();
                        guard.staging_fences.clear();

                        let staging_desc = D3D11_TEXTURE2D_DESC {
                            Width: w,
                            Height: h,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                            Usage: D3D11_USAGE_STAGING,
                            BindFlags: 0,
                            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                            MiscFlags: 0,
                        };

                        for _ in 0..4 {
                            let mut tex: Option<ID3D11Texture2D> = None;
                            let hr = unsafe { guard.device.device.CreateTexture2D(&staging_desc, None, Some(&mut tex)) };
                            if hr.is_err() || tex.is_none() {
                                let _ = on_error_clone.call(
                                    ErrorPayload {
                                        component: "capture_session".to_string(),
                                        hresult: hr.err().map(|e| e.code().0).unwrap_or(0),
                                        message: "failed to create staging texture".to_string(),
                                    },
                                    ThreadsafeFunctionCallMode::NonBlocking,
                                );
                                guard.pool.release(frame.slot);
                                return;
                            }
                            guard.staging_textures.push(tex.unwrap());

                            let query_desc = D3D11_QUERY_DESC {
                                Query: D3D11_QUERY_EVENT,
                                MiscFlags: 0,
                            };
                            let mut fence: Option<ID3D11Query> = None;
                            let _ = unsafe { guard.device.device.CreateQuery(&query_desc, Some(&mut fence)) };
                            guard.staging_fences.push(fence.unwrap());
                        }
                        guard.staging_w = w;
                        guard.staging_h = h;
                        guard.staging_idx = 0;
                    }

                    let idx = guard.staging_idx as usize % guard.staging_textures.len();
                    guard.staging_idx = guard.staging_idx.wrapping_add(1);

                    let staging = &guard.staging_textures[idx];
                    let fence = &guard.staging_fences[idx];

                    // Copy from pool texture to staging.
                    let box_ = D3D11_BOX {
                        left: 0, top: 0, front: 0,
                        right: w, bottom: h, back: 1,
                    };
                    unsafe {
                        guard.device.context.CopySubresourceRegion(
                            staging, 0, 0, 0, 0,
                            &frame.slot.texture, 0, Some(&box_),
                        );
                        guard.device.context.End(fence);
                        guard.device.context.Flush();
                    }

                    // Release the BGRA slot back to the pool immediately.
                    guard.pool.release(frame.slot);

                    // Wait for GPU to finish the copy.
                    loop {
                        let mut data: u32 = 0;
                        let hr = unsafe {
                            guard.device.context.GetData(
                                fence,
                                Some(&mut data as *mut u32 as *mut _),
                                std::mem::size_of::<u32>() as u32,
                                D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                            )
                        };
                        if hr.is_ok() {
                            break;
                        }
                        std::thread::yield_now();
                    }

                    // Map and read pixels.
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    let map_result = unsafe {
                        guard.device.context.Map(
                            staging,
                            0,
                            D3D11_MAP_READ,
                            0,
                            Some(&mut mapped),
                        )
                    };

                    if map_result.is_err() {
                        let _ = on_error_clone.call(
                            ErrorPayload {
                                component: "capture_session".to_string(),
                                hresult: map_result.err().map(|e| e.code().0).unwrap_or(0),
                                message: "Map staging texture failed".to_string(),
                            },
                            ThreadsafeFunctionCallMode::NonBlocking,
                        );
                        return;
                    }

                    let row_pitch = mapped.RowPitch;
                    let total_bytes = row_pitch * h;
                    let pixels = unsafe {
                        std::slice::from_raw_parts(mapped.pData as *const u8, total_bytes as usize)
                    }.to_vec();

                    unsafe { guard.device.context.Unmap(staging, 0); }

                    let ts = frame.timestamp_ns;
                    let sc = frame.size_changed;
                    // Drop the mutex guard before calling TSFN.
                    drop(guard);

                    let _ = on_frame_clone.call(
                        FramePayload {
                            timestamp_ns: ts,
                            width: w,
                            height: h,
                            format: "bgra8",
                            size_changed: sc,
                            data: Some(pixels),
                            stride: row_pitch,
                        },
                        ThreadsafeFunctionCallMode::NonBlocking,
                    );
                    delivered.fetch_add(1, Ordering::Relaxed);
                }
                DeliveryMode::Encoded => {
                    // Encoded mode: color-convert BGRA->NV12, then encode.
                    let mut guard = inner_mutex.lock();
                    let w = frame.width;
                    let h = frame.height;

                    // If encoder already failed, don't retry — just drop frames silently.
                    if guard.encoder_failed {
                        guard.pool.release(frame.slot);
                        return;
                    }

                    // Lazy-init or re-init encoder on size change.
                    if guard.enc_width != w || guard.enc_height != h {
                        // Tear down old encoder if any.
                        if let Some(ref enc) = guard.encoder {
                            enc.stop();
                        }
                        guard.encoder = None;
                        guard.color = None;
                        guard.nv12_texture = None;

                        let cs = if guard.enc_cfg.hdr10 {
                            ColorSpace::Rec2020Pq
                        } else {
                            ColorSpace::Rec709Sdr
                        };

                        let nv12_format = if guard.enc_cfg.hdr10 {
                            DXGI_FORMAT_P010
                        } else {
                            DXGI_FORMAT_NV12
                        };

                        // Create VideoProcessor for color conversion.
                        match VideoProcessor::new(
                            &guard.device.device,
                            &guard.device.context,
                            w, h,
                            cs,
                        ) {
                            Ok(vp) => guard.color = Some(DummySend(vp)),
                            Err(e) => {
                                let _ = on_error_clone.call(
                                    ErrorPayload {
                                        component: "video_processor".to_string(),
                                        hresult: 0,
                                        message: e.to_string(),
                                    },
                                    ThreadsafeFunctionCallMode::NonBlocking,
                                );
                                guard.encoder_failed = true;
                                guard.pool.release(frame.slot);
                                return;
                            }
                        }

                        // Create NV12/P010 intermediate texture.
                        let nv12_desc = D3D11_TEXTURE2D_DESC {
                            Width: w,
                            Height: h,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: nv12_format,
                            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                            Usage: D3D11_USAGE_DEFAULT,
                            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                            CPUAccessFlags: 0,
                            MiscFlags: 0,
                        };
                        let mut nv12_tex: Option<ID3D11Texture2D> = None;
                        let hr = unsafe { guard.device.device.CreateTexture2D(&nv12_desc, None, Some(&mut nv12_tex)) };
                        if hr.is_err() || nv12_tex.is_none() {
                            let _ = on_error_clone.call(
                                ErrorPayload {
                                    component: "capture_session".to_string(),
                                    hresult: hr.err().map(|e| e.code().0).unwrap_or(0),
                                    message: "failed to create NV12 texture".to_string(),
                                },
                                ThreadsafeFunctionCallMode::NonBlocking,
                            );
                            guard.encoder_failed = true;
                            guard.pool.release(frame.slot);
                            return;
                        }
                        guard.nv12_texture = nv12_tex;

                        // Create MfEncoder.
                        let mut enc_cfg = guard.enc_cfg.clone();
                        enc_cfg.width = w;
                        enc_cfg.height = h;

                        match MfEncoder::new() {
                            Ok(mut enc) => {
                                if let Err(e) = enc.initialize(&guard.device.device, enc_cfg) {
                                    let _ = on_error_clone.call(
                                        ErrorPayload {
                                            component: "mf_encoder".to_string(),
                                            hresult: 0,
                                            message: e.to_string(),
                                        },
                                        ThreadsafeFunctionCallMode::NonBlocking,
                                    );
                                    guard.encoder_failed = true;
                                    guard.pool.release(frame.slot);
                                    return;
                                }

                                // Wire up encoder output callback -> on_encoded TSFN.
                                let on_enc = on_encoded_clone.clone();
                                let enc_counter = encoded_units;
                                let out_cb = Box::new(move |au: wincap_core::mf_encoder::EncodedAccessUnit| {
                                    let _ = on_enc.call(
                                        EncodedPayload {
                                            data: au.data,
                                            timestamp_ns: au.timestamp_ns,
                                            keyframe: au.keyframe,
                                        },
                                        ThreadsafeFunctionCallMode::NonBlocking,
                                    );
                                    // SAFETY: same lifetime guarantee as other atomics here.
                                    enc_counter.fetch_add(1, Ordering::Relaxed);
                                });

                                let on_err = on_error_clone.clone();
                                let err_enc_cb = Box::new(move |component: &'static str, hr: i32, msg: &str| {
                                    let _ = on_err.call(
                                        ErrorPayload {
                                            component: component.to_string(),
                                            hresult: hr,
                                            message: msg.to_string(),
                                        },
                                        ThreadsafeFunctionCallMode::NonBlocking,
                                    );
                                });

                                if let Err(e) = enc.start(out_cb, err_enc_cb) {
                                    let _ = on_error_clone.call(
                                        ErrorPayload {
                                            component: "mf_encoder".to_string(),
                                            hresult: 0,
                                            message: e.to_string(),
                                        },
                                        ThreadsafeFunctionCallMode::NonBlocking,
                                    );
                                    guard.encoder_failed = true;
                                    guard.pool.release(frame.slot);
                                    return;
                                }

                                guard.encoder = Some(DummySend(enc));
                            }
                            Err(e) => {
                                let _ = on_error_clone.call(
                                    ErrorPayload {
                                        component: "mf_encoder".to_string(),
                                        hresult: 0,
                                        message: e.to_string(),
                                    },
                                    ThreadsafeFunctionCallMode::NonBlocking,
                                );
                                guard.encoder_failed = true;
                                guard.pool.release(frame.slot);
                                return;
                            }
                        }

                        guard.enc_width = w;
                        guard.enc_height = h;
                    }

                    // Color convert BGRA -> NV12.
                    let nv12_tex = guard.nv12_texture.as_ref().unwrap();
                    if let Some(ref color) = guard.color {
                        if let Err(e) = color.convert(&frame.slot.texture, nv12_tex) {
                            let _ = on_error_clone.call(
                                ErrorPayload {
                                    component: "video_processor".to_string(),
                                    hresult: 0,
                                    message: e.to_string(),
                                },
                                ThreadsafeFunctionCallMode::NonBlocking,
                            );
                            guard.pool.release(frame.slot);
                            return;
                        }
                    }

                    // Encode the NV12 frame.
                    if let Some(ref enc) = guard.encoder {
                        enc.encode_frame(nv12_tex, frame.timestamp_ns, FrameOptions::default());
                    }

                    // Release the BGRA slot back to the pool.
                    guard.pool.release(frame.slot);
                    delivered.fetch_add(1, Ordering::Relaxed);
                }
            }
        });

        source.start(frame_cb, err_cb).map_err(to_napi_err)?;
        Ok(())
    }

    #[napi]
    pub fn stop(&self) -> Result<()> {
        if !self.running.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let mut inner = self.inner.lock();
        if let Some(ref mut source) = inner.source {
            source.stop();
        }
        // Stop encoder if running.
        if let Some(ref enc) = inner.encoder {
            enc.stop();
        }
        Ok(())
    }

    #[napi]
    pub fn get_stats(&self) -> CaptureStatsJs {
        CaptureStatsJs {
            delivered_frames: BigInt::from(self.delivered_frames.load(Ordering::Relaxed)),
            dropped_frames: BigInt::from(self.dropped_frames.load(Ordering::Relaxed)),
            encoded_units: BigInt::from(self.encoded_units.load(Ordering::Relaxed)),
        }
    }

    #[napi]
    pub fn request_keyframe(&self) {
        let inner = self.inner.lock();
        if let Some(ref enc) = inner.encoder {
            enc.request_keyframe();
        }
    }

    #[napi]
    pub fn set_bitrate(&self, bps: u32) {
        let inner = self.inner.lock();
        if let Some(ref enc) = inner.encoder {
            enc.set_bitrate(bps);
        }
    }
}

fn to_napi_err(e: WincapError) -> Error {
    Error::from_reason(e.to_string())
}
