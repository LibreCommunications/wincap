use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi::JsFunction;
use napi_derive::napi;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Graphics::DirectX::DirectXPixelFormat;

use wincap_core::clock;
use wincap_core::d3d_device::D3DDevice;
use wincap_core::error::WincapError;
use wincap_core::frame_pool::FramePool;
use wincap_core::mf_encoder::{EncoderConfig, VideoCodec};
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
    device: DummySend<D3DDevice>,
    pool: DummySend<FramePool>,
    source: Option<DummySend<WgcSource>>,
    delivery: DeliveryMode,
    enc_cfg: EncoderConfig,
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

        // Create D3D device and frame pool.
        let device = D3DDevice::create(LUID::default()).map_err(to_napi_err)?;

        let hdr_capture = delivery == DeliveryMode::Encoded && enc_cfg.hdr10;
        let format = if hdr_capture {
            DXGI_FORMAT_R16G16B16A16_FLOAT
        } else {
            DXGI_FORMAT_B8G8R8A8_UNORM
        };

        let desc = D3D11_TEXTURE2D_DESC {
            Width: 1,
            Height: 1,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };

        let mut pool = FramePool::new();
        pool.init(&device.device, 4, desc, false).map_err(to_napi_err)?;

        let mut wgc_opts = WgcOptions::default();
        if let Some(v) = opts.include_cursor { wgc_opts.include_cursor = v; }
        if let Some(v) = opts.border_required { wgc_opts.border_required = v; }
        if hdr_capture {
            wgc_opts.pixel_format = DirectXPixelFormat::R16G16B16A16Float;
        }

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
        let source = inner.source.as_mut()
            .ok_or_else(|| Error::from_reason("capture source not initialized"))?;

        let _on_frame = self.on_frame.clone();
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
        let frame_cb = Box::new(move |frame: wincap_core::wgc_source::CapturedFrame<'_>| {
            let _ = on_frame_clone.call(
                FramePayload {
                    timestamp_ns: frame.timestamp_ns,
                    width: frame.width,
                    height: frame.height,
                    format: "bgra8",
                    size_changed: frame.size_changed,
                },
                ThreadsafeFunctionCallMode::NonBlocking,
            );
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
        // Encoder integration will be added in follow-up.
    }

    #[napi]
    pub fn set_bitrate(&self, _bps: u32) {
        // Encoder integration will be added in follow-up.
    }
}

fn to_napi_err(e: WincapError) -> Error {
    Error::from_reason(e.to_string())
}
