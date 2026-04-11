use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use windows::core::{implement, Interface, GUID};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;

use crate::error::{hr_call, WincapError, WincapResult};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VideoCodec {
    H264,
    HEVC,
    AV1,
}

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub keyframe_interval_ms: u32,
    pub hdr10: bool,
    pub ltr_count: u32,
    pub intra_refresh: bool,
    pub intra_refresh_period: u32,
    pub roi_enabled: bool,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            codec: VideoCodec::H264,
            width: 0,
            height: 0,
            fps: 60,
            bitrate_bps: 6_000_000,
            keyframe_interval_ms: 2000,
            hdr10: false,
            ltr_count: 0,
            intra_refresh: false,
            intra_refresh_period: 60,
            roi_enabled: false,
        }
    }
}

/// A single encoded access unit (one or more NAL units).
pub struct EncodedAccessUnit {
    pub data: Vec<u8>,
    pub timestamp_ns: u64,
    pub keyframe: bool,
}

/// Per-frame options for LTR and ROI.
#[derive(Clone, Default)]
pub struct FrameOptions {
    pub mark_ltr: i32,  // -1 = none
    pub use_ltr: i32,   // -1 = none
    pub roi_rects: Vec<i32>, // groups of 4 (l,t,r,b)
}

pub type EncodedCallback = Box<dyn Fn(EncodedAccessUnit) + Send + Sync>;
pub type EncoderErrorCallback = Box<dyn Fn(&'static str, i32, &str) + Send + Sync>;

struct PendingInput {
    tex: ID3D11Texture2D,
    timestamp_ns: u64,
    opts: FrameOptions,
}

// SAFETY: COM objects are thread-safe, PendingInput is only moved across threads.
unsafe impl Send for PendingInput {}

struct EncoderInner {
    cfg: EncoderConfig,
    mft: IMFTransform,
    event_gen: IMFMediaEventGenerator,
    codec_api: Option<ICodecAPI>,
    running: AtomicBool,
    force_keyframe: AtomicBool,
    pending: Mutex<VecDeque<PendingInput>>,
    on_output: Mutex<Option<EncodedCallback>>,
    on_error: Mutex<Option<EncoderErrorCallback>>,
}

// SAFETY: All interior mutation is behind Mutex or atomics.
unsafe impl Send for EncoderInner {}
unsafe impl Sync for EncoderInner {}

pub struct MfEncoder {
    inner: Option<Arc<EncoderInner>>,
    dxgi_manager: Option<IMFDXGIDeviceManager>,
    dxgi_token: u32,
    callback: Option<IMFAsyncCallback>,
}

// SAFETY: MfEncoder only accessed from one thread at a time.
unsafe impl Send for MfEncoder {}

#[implement(IMFAsyncCallback)]
struct AsyncCallback {
    inner: Arc<EncoderInner>,
}

impl IMFAsyncCallback_Impl for AsyncCallback_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(
        &self,
        presult: windows::core::Ref<'_, IMFAsyncResult>,
    ) -> windows::core::Result<()> {
        if let Some(result) = &*presult {
            invoke_impl(&self.inner, result);
        }
        Ok(())
    }
}

impl MfEncoder {
    pub fn new() -> WincapResult<Self> {
        hr_call!("mf_encoder", unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) });
        Ok(Self {
            inner: None,
            dxgi_manager: None,
            dxgi_token: 0,
            callback: None,
        })
    }

    pub fn initialize(&mut self, device: &ID3D11Device5, cfg: EncoderConfig) -> WincapResult<()> {
        if cfg.hdr10 && cfg.codec == VideoCodec::H264 {
            return Err(WincapError::General {
                component: "mf_encoder",
                message: "HDR10 requires HEVC or AV1 (no 10-bit H.264)".into(),
            });
        }

        // 1. DXGI device manager for GPU-resident input.
        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        hr_call!("mf_encoder", unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) });
        let manager = manager.ok_or_else(|| WincapError::General {
            component: "mf_encoder",
            message: "MFCreateDXGIDeviceManager returned None".into(),
        })?;
        hr_call!("mf_encoder", unsafe { manager.ResetDevice(device, token) });
        self.dxgi_manager = Some(manager.clone());
        self.dxgi_token = token;

        // 2. Locate a vendor-matched hardware async MFT.
        let subtype = match cfg.codec {
            VideoCodec::H264 => MFVideoFormat_H264,
            VideoCodec::HEVC => MFVideoFormat_HEVC,
            VideoCodec::AV1 => MFVideoFormat_AV1,
        };

        let out_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: subtype,
        };
        let enum_flags =
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER;

        let mut activate_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut activate_count: u32 = 0;
        hr_call!("mf_encoder", unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                enum_flags,
                None,
                Some(&out_info),
                &mut activate_ptr,
                &mut activate_count,
            )
        });

        let activates = if activate_count > 0 && !activate_ptr.is_null() {
            let slice = unsafe { std::slice::from_raw_parts(activate_ptr, activate_count as usize) };
            let vec: Vec<IMFActivate> = slice.iter().filter_map(|opt| opt.clone()).collect();
            unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(activate_ptr as *const _)) };
            vec
        } else {
            Vec::new()
        };

        if activates.is_empty() {
            return Err(WincapError::General {
                component: "mf_encoder",
                message: "no hardware async encoder available for requested codec".into(),
            });
        }

        let vendor = get_adapter_vendor_id(device);
        let pick = pick_best_activate(&activates, vendor);

        let mft: IMFTransform =
            hr_call!("mf_encoder", unsafe { activates[pick].ActivateObject() });

        // 3. Bind DXGI manager.
        hr_call!("mf_encoder", unsafe {
            mft.ProcessMessage(
                MFT_MESSAGE_SET_D3D_MANAGER,
                &manager as *const _ as usize,
            )
        });

        // 4. Async unlock + low-latency.
        let attrs: IMFAttributes = hr_call!("mf_encoder", unsafe { mft.GetAttributes() });
        hr_call!("mf_encoder", unsafe {
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
        });
        hr_call!("mf_encoder", unsafe { attrs.SetUINT32(&MF_LOW_LATENCY, 1) });

        // 5. Output media type.
        let out_type: IMFMediaType = hr_call!("mf_encoder", unsafe { MFCreateMediaType() });
        unsafe {
            hr_call!("mf_encoder", out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video));
            hr_call!("mf_encoder", out_type.SetGUID(&MF_MT_SUBTYPE, &subtype));
            hr_call!("mf_encoder", out_type.SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate_bps));
            hr_call!("mf_encoder", out_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_2u32(cfg.width, cfg.height)));
            hr_call!("mf_encoder", out_type.SetUINT64(&MF_MT_FRAME_RATE, pack_2u32(cfg.fps, 1)));
            hr_call!("mf_encoder", out_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_2u32(1, 1)));
            hr_call!("mf_encoder", out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32));

            if cfg.codec == VideoCodec::H264 {
                hr_call!("mf_encoder", out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32));
            } else if cfg.codec == VideoCodec::HEVC {
                let profile = if cfg.hdr10 { 2u32 } else { 1u32 }; // Main10 / Main
                hr_call!("mf_encoder", out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, profile));
            }

            if cfg.hdr10 {
                hr_call!("mf_encoder", out_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT2020.0 as u32));
                hr_call!("mf_encoder", out_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_2084.0 as u32));
                hr_call!("mf_encoder", out_type.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT2020_10.0 as u32));
                hr_call!("mf_encoder", out_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32));
            }

            hr_call!("mf_encoder", mft.SetOutputType(0, &out_type, 0));
        }

        // 6. Input media type — NV12 (SDR) or P010 (HDR10).
        let in_type: IMFMediaType = hr_call!("mf_encoder", unsafe { MFCreateMediaType() });
        let in_subtype = if cfg.hdr10 {
            MFVideoFormat_P010
        } else {
            MFVideoFormat_NV12
        };
        unsafe {
            hr_call!("mf_encoder", in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video));
            hr_call!("mf_encoder", in_type.SetGUID(&MF_MT_SUBTYPE, &in_subtype));
            hr_call!("mf_encoder", in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_2u32(cfg.width, cfg.height)));
            hr_call!("mf_encoder", in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_2u32(cfg.fps, 1)));
            hr_call!("mf_encoder", in_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_2u32(1, 1)));
            hr_call!("mf_encoder", in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32));
            hr_call!("mf_encoder", mft.SetInputType(0, &in_type, 0));
        }

        // 7. ICodecAPI tuning — all best-effort.
        let codec_api: Option<ICodecAPI> = mft.cast().ok();
        if let Some(ref api) = codec_api {
            set_u32(api, &CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_LowDelayVBR.0 as u32);
            set_u32(api, &CODECAPI_AVEncCommonMeanBitRate, cfg.bitrate_bps);
            set_u32(api, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
            set_u32(
                api,
                &CODECAPI_AVEncMPVGOPSize,
                (cfg.fps * cfg.keyframe_interval_ms / 1000).max(1),
            );
            set_bool(api, &CODECAPI_AVLowLatencyMode, true);
            set_bool(api, &CODECAPI_AVEncCommonRealTime, true);

            if cfg.codec == VideoCodec::H264 {
                set_bool(api, &CODECAPI_AVEncH264CABACEnable, true);
            }

            if cfg.ltr_count > 0 {
                let ltr_pack = (0x0001u32 << 16) | (cfg.ltr_count & 0xFFFF);
                set_u32(api, &CODECAPI_AVEncVideoLTRBufferControl, ltr_pack);
            }

            if cfg.roi_enabled {
                set_bool(api, &CODECAPI_AVEncVideoROIEnabled, true);
            }

            set_u32(api, &CODECAPI_AVEncNumWorkerThreads, 0);
        }

        // 8. Event generator.
        let event_gen: IMFMediaEventGenerator = hr_call!("mf_encoder", mft.cast());

        let inner = Arc::new(EncoderInner {
            cfg,
            mft,
            event_gen,
            codec_api,
            running: AtomicBool::new(false),
            force_keyframe: AtomicBool::new(false),
            pending: Mutex::new(VecDeque::new()),
            on_output: Mutex::new(None),
            on_error: Mutex::new(None),
        });

        let callback_impl = AsyncCallback {
            inner: Arc::clone(&inner),
        };
        let callback: IMFAsyncCallback = callback_impl.into();

        self.inner = Some(inner);
        self.callback = Some(callback);

        Ok(())
    }

    pub fn start(&self, out: EncodedCallback, err: EncoderErrorCallback) -> WincapResult<()> {
        let inner = self.inner.as_ref().ok_or_else(|| WincapError::General {
            component: "mf_encoder",
            message: "not initialized".into(),
        })?;

        if inner.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        *inner.on_output.lock() = Some(out);
        *inner.on_error.lock() = Some(err);

        unsafe {
            hr_call!("mf_encoder", inner.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0));
            hr_call!("mf_encoder", inner.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0));
        }

        let cb = self.callback.as_ref().unwrap();
        hr_call!("mf_encoder", unsafe { inner.event_gen.BeginGetEvent(cb, None) });

        Ok(())
    }

    pub fn stop(&self) {
        let Some(inner) = &self.inner else { return };
        if !inner.running.swap(false, Ordering::SeqCst) {
            return;
        }
        unsafe {
            let _ = inner.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = inner.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = inner.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        *inner.on_output.lock() = None;
        *inner.on_error.lock() = None;
        inner.pending.lock().clear();
    }

    /// Submit an input texture for encoding.
    pub fn encode_frame(
        &self,
        surface: &ID3D11Texture2D,
        timestamp_ns: u64,
        opts: FrameOptions,
    ) {
        let Some(inner) = &self.inner else { return };
        if !inner.running.load(Ordering::Acquire) {
            return;
        }
        inner.pending.lock().push_back(PendingInput {
            tex: surface.clone(),
            timestamp_ns,
            opts,
        });
    }

    pub fn request_keyframe(&self) {
        if let Some(inner) = &self.inner {
            inner.force_keyframe.store(true, Ordering::Release);
        }
    }

    pub fn set_bitrate(&self, bps: u32) {
        if let Some(inner) = &self.inner {
            if let Some(ref api) = inner.codec_api {
                set_u32(api, &CODECAPI_AVEncCommonMeanBitRate, bps);
            }
        }
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        self.stop();
        unsafe { let _ = MFShutdown(); }
    }
}

fn invoke_impl(inner: &Arc<EncoderInner>, result: &IMFAsyncResult) {
    if !inner.running.load(Ordering::Acquire) {
        return;
    }

    let evt = match unsafe { inner.event_gen.EndGetEvent(result) } {
        Ok(e) => e,
        Err(e) => {
            if let Some(cb) = inner.on_error.lock().as_ref() {
                cb("mf_encoder", e.code().0, "EndGetEvent failed");
            }
            return;
        }
    };

    let event_type = unsafe { evt.GetType() }.unwrap_or(0);

    let result = match event_type {
        t if t == METransformNeedInput.0 as u32 => on_need_input(inner),
        t if t == METransformHaveOutput.0 as u32 => on_have_output(inner),
        _ => Ok(()),
    };

    if let Err(e) = result {
        if let Some(cb) = inner.on_error.lock().as_ref() {
            match &e {
                WincapError::HResult { component, hr, context } => cb(component, *hr, context),
                WincapError::General { component, message } => cb(component, 0, message),
            }
        }
    }

    // Continue listening for events.
    if inner.running.load(Ordering::Acquire) {
        // We need to get the callback again. Unfortunately we don't have it here,
        // so we use BeginGetEvent with the event generator's existing callback.
        // This is a limitation — we'll store the callback in the inner.
        // For now, we re-create a callback pointing to this inner.
        let cb: IMFAsyncCallback = AsyncCallback {
            inner: Arc::clone(inner),
        }
        .into();
        let _ = unsafe { inner.event_gen.BeginGetEvent(&cb, None) };
    }
}

fn on_need_input(inner: &EncoderInner) -> WincapResult<()> {
    let input = match inner.pending.lock().pop_front() {
        Some(v) => v,
        None => return Ok(()),
    };

    let sample: IMFSample = hr_call!("mf_encoder", unsafe { MFCreateSample() });
    let buf: IMFMediaBuffer = hr_call!("mf_encoder", unsafe {
        MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &input.tex, 0, false)
    });
    hr_call!("mf_encoder", unsafe { sample.AddBuffer(&buf) });

    let pts_hns = (input.timestamp_ns / 100) as i64;
    hr_call!("mf_encoder", unsafe { sample.SetSampleTime(pts_hns) });
    let duration = 10_000_000i64 / inner.cfg.fps.max(1) as i64;
    hr_call!("mf_encoder", unsafe { sample.SetSampleDuration(duration) });

    if inner.force_keyframe.swap(false, Ordering::AcqRel) {
        let _ = unsafe { sample.SetUINT32(&MFSampleExtension_CleanPoint, 1) };
    }

    // LTR markers
    if input.opts.mark_ltr >= 0 {
        let _ = unsafe {
            sample.SetUINT32(
                &MFSampleExtension_LongTermReferenceFrameInfo,
                input.opts.mark_ltr as u32,
            )
        };
    }
    if input.opts.use_ltr >= 0 {
        if let Some(ref api) = inner.codec_api {
            set_u32(api, &CODECAPI_AVEncVideoUseLTRFrame, input.opts.use_ltr as u32);
        }
    }

    // ROI rectangles
    if !input.opts.roi_rects.is_empty() {
        let bytes = unsafe {
            std::slice::from_raw_parts(
                input.opts.roi_rects.as_ptr() as *const u8,
                input.opts.roi_rects.len() * std::mem::size_of::<i32>(),
            )
        };
        let _ = unsafe { sample.SetBlob(&MFSampleExtension_ROIRectangle, bytes) };
    }

    hr_call!("mf_encoder", unsafe { inner.mft.ProcessInput(0, &sample, 0) });
    Ok(())
}

fn on_have_output(inner: &EncoderInner) -> WincapResult<()> {
    let info = hr_call!("mf_encoder", unsafe { inner.mft.GetOutputStreamInfo(0) });

    let mut out_buf = [MFT_OUTPUT_DATA_BUFFER::default()];
    let mut status = 0u32;

    if (info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) == 0 {
        let s: IMFSample = hr_call!("mf_encoder", unsafe { MFCreateSample() });
        let buf: IMFMediaBuffer =
            hr_call!("mf_encoder", unsafe { MFCreateMemoryBuffer(info.cbSize) });
        hr_call!("mf_encoder", unsafe { s.AddBuffer(&buf) });
        out_buf[0].pSample = std::mem::ManuallyDrop::new(Some(s));
    }

    let hr = unsafe { inner.mft.ProcessOutput(0, &mut out_buf, &mut status) };

    match hr {
        Ok(()) => {}
        Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
        Err(e) => {
            return Err(WincapError::HResult {
                component: "mf_encoder",
                hr: e.code().0,
                context: "ProcessOutput".into(),
            });
        }
    }

    let got_sample = std::mem::ManuallyDrop::into_inner(
        std::mem::take(&mut out_buf[0].pSample),
    );
    let got_sample = got_sample.ok_or_else(|| WincapError::General {
        component: "mf_encoder",
        message: "ProcessOutput returned no sample".into(),
    })?;

    // Clean up events if any
    let _events = std::mem::ManuallyDrop::into_inner(
        std::mem::take(&mut out_buf[0].pEvents),
    );

    let pts_hns = unsafe { got_sample.GetSampleTime() }.unwrap_or(0);
    let keyframe = unsafe { got_sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0);

    let buf: IMFMediaBuffer =
        hr_call!("mf_encoder", unsafe { got_sample.ConvertToContiguousBuffer() });

    let mut data_ptr = std::ptr::null_mut::<u8>();
    let mut max_len = 0u32;
    let mut cur_len = 0u32;
    hr_call!("mf_encoder", unsafe {
        buf.Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
    });

    let data = unsafe { std::slice::from_raw_parts(data_ptr, cur_len as usize) }.to_vec();
    let _ = unsafe { buf.Unlock() };

    if let Some(cb) = inner.on_output.lock().as_ref() {
        cb(EncodedAccessUnit {
            data,
            timestamp_ns: (pts_hns as u64) * 100,
            keyframe: keyframe != 0,
        });
    }

    Ok(())
}

/// Pack two u32 values into a u64 (high << 32 | low), equivalent to MFSetAttributeSize/Ratio.
fn pack_2u32(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | (low as u64)
}

// Helper: set a UINT32 on ICodecAPI (best-effort).
fn set_u32(api: &ICodecAPI, prop: &GUID, value: u32) {
    let var = VARIANT::from(value);
    let _ = unsafe { api.SetValue(prop, &var) };
}

// Helper: set a BOOL on ICodecAPI (best-effort).
fn set_bool(api: &ICodecAPI, prop: &GUID, b: bool) {
    let var = VARIANT::from(b);
    let _ = unsafe { api.SetValue(prop, &var) };
}

fn get_adapter_vendor_id(device: &ID3D11Device5) -> u32 {
    let dxgi_device: IDXGIDevice = match device.cast() {
        Ok(d) => d,
        Err(_) => return 0,
    };
    let adapter: IDXGIAdapter = match unsafe { dxgi_device.GetAdapter() } {
        Ok(a) => a,
        Err(_) => return 0,
    };
    let desc = match unsafe { adapter.GetDesc() } {
        Ok(d) => d,
        Err(_) => return 0,
    };
    desc.VendorId
}

fn pick_best_activate(activates: &[IMFActivate], vendor_id: u32) -> usize {
    if vendor_id == 0 || activates.len() <= 1 {
        return 0;
    }

    let want = format!("VEN_{:04X}", vendor_id);
    for (i, activate) in activates.iter().enumerate() {
        let mut pwsz = windows::core::PWSTR::null();
        let mut cch = 0u32;
        if unsafe {
            activate.GetAllocatedString(&MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, &mut pwsz, &mut cch)
        }.is_ok() {
            let vendor_str = unsafe { pwsz.to_string().unwrap_or_default() };
            if vendor_str.eq_ignore_ascii_case(&want) {
                return i;
            }
        }
    }
    0
}
