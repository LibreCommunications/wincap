use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use windows::core::{implement, IUnknown, Interface, HRESULT, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{WAVE_FORMAT_EXTENSIBLE, SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::*;

use crate::audio_format::AudioFormat;
use crate::clock;
use crate::error::{hr_call, WincapError, WincapResult};
use crate::mmcss::MmcssScope;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LoopbackMode {
    SystemDefault,
    ProcessTree,
}

pub struct WasapiLoopbackOptions {
    pub mode: LoopbackMode,
    pub target_pid: u32,
    pub include_tree: bool,
}

impl Default for WasapiLoopbackOptions {
    fn default() -> Self {
        Self {
            mode: LoopbackMode::SystemDefault,
            target_pid: 0,
            include_tree: true,
        }
    }
}

/// An audio chunk delivered to the consumer.
pub struct AudioChunk {
    pub data: *const f32,
    pub frame_count: u32,
    pub channels: u32,
    pub sample_rate: u32,
    pub timestamp_ns: u64,
    pub silent: bool,
    pub discontinuity: bool,
    /// Call this when done with the chunk to return the buffer to the pool.
    pub release: Box<dyn FnOnce() + Send>,
}

// SAFETY: The data pointer is valid until release is called.
unsafe impl Send for AudioChunk {}

pub type AudioCallback = Box<dyn Fn(AudioChunk) + Send + Sync>;
pub type AudioErrorCallback = Box<dyn Fn(&'static str, i32, &str) + Send + Sync>;

const POOL_SIZE: usize = 16;

struct PoolBuffer {
    data: parking_lot::Mutex<Vec<f32>>,
    in_use: AtomicU32,
}

struct WasapiInner {
    opts: WasapiLoopbackOptions,
    format: parking_lot::Mutex<AudioFormat>,
    running: AtomicBool,
    pool: Vec<PoolBuffer>,
    cb: parking_lot::Mutex<Option<AudioCallback>>,
    err_cb: parking_lot::Mutex<Option<AudioErrorCallback>>,
}

pub struct WasapiLoopback {
    inner: Arc<WasapiInner>,
    thread: Option<std::thread::JoinHandle<()>>,
    stop_event: HANDLE,
}

// SAFETY: Thread handle and event handle are thread-safe.
unsafe impl Send for WasapiLoopback {}
unsafe impl Sync for WasapiLoopback {}

// COM completion handler for ActivateAudioInterfaceAsync.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivateHandler {
    done_event: HANDLE,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivateHandler_Impl {
    fn ActivateCompleted(
        &self,
        _operation: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        unsafe { SetEvent(self.done_event) }
    }
}

impl WasapiLoopback {
    pub fn new(opts: WasapiLoopbackOptions) -> Self {
        let mut pool = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            pool.push(PoolBuffer {
                data: parking_lot::Mutex::new(Vec::new()),
                in_use: AtomicU32::new(0),
            });
        }

        Self {
            inner: Arc::new(WasapiInner {
                opts,
                format: parking_lot::Mutex::new(AudioFormat::default()),
                running: AtomicBool::new(false),
                pool,
                cb: parking_lot::Mutex::new(None),
                err_cb: parking_lot::Mutex::new(None),
            }),
            thread: None,
            stop_event: HANDLE::default(),
        }
    }

    pub fn format(&self) -> AudioFormat {
        self.inner.format.lock().clone()
    }

    pub fn start(&mut self, cb: AudioCallback, err_cb: AudioErrorCallback) -> WincapResult<()> {
        if self.inner.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        *self.inner.cb.lock() = Some(cb);
        *self.inner.err_cb.lock() = Some(err_cb);

        self.stop_event = unsafe { CreateEventW(None, true, false, None) }.map_err(|e| {
            self.inner.running.store(false, Ordering::SeqCst);
            WincapError::HResult {
                component: "wasapi_loopback",
                hr: e.code().0,
                context: "CreateEvent for stop signal".into(),
            }
        })?;

        let inner = Arc::clone(&self.inner);
        let stop_event = self.stop_event;

        // SAFETY: HANDLE is a raw pointer wrapper but kernel object handles are
        // inherently thread-safe (they're just integers indexing a kernel table).
        let stop_raw = stop_event.0 as usize;
        self.thread = Some(std::thread::spawn(move || {
            thread_main(inner, HANDLE(stop_raw as *mut _));
        }));

        Ok(())
    }

    pub fn stop(&mut self) {
        if !self.inner.running.swap(false, Ordering::SeqCst) {
            return;
        }
        if !self.stop_event.is_invalid() {
            let _ = unsafe { SetEvent(self.stop_event) };
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if !self.stop_event.is_invalid() {
            let _ = unsafe { CloseHandle(self.stop_event) };
            self.stop_event = HANDLE::default();
        }
        *self.inner.cb.lock() = None;
        *self.inner.err_cb.lock() = None;
    }
}

impl Drop for WasapiLoopback {
    fn drop(&mut self) {
        self.stop();
    }
}

fn acquire_buffer(pool: &[PoolBuffer], needed_floats: usize) -> Option<usize> {
    for (i, buf) in pool.iter().enumerate() {
        let expected = 0u32;
        if buf
            .in_use
            .compare_exchange(expected, 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let mut data = buf.data.lock();
            if data.len() < needed_floats {
                data.resize(needed_floats, 0.0);
            }
            return Some(i);
        }
    }
    None
}

fn thread_main(inner: Arc<WasapiInner>, stop_event: HANDLE) {
    let co = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let _mmcss = MmcssScope::new("Pro Audio");

    let result: WincapResult<(IAudioClient, IAudioCaptureClient, HANDLE)> = (|| {
        let client = activate(&inner.opts)?;
        let (capture, event, format) = initialize(&client, &inner.opts)?;
        *inner.format.lock() = format;
        hr_call!("wasapi_loopback", unsafe { client.Start() });
        Ok((client, capture, event))
    })();

    let (client, capture, event) = match result {
        Ok(v) => v,
        Err(e) => {
            if let Some(cb) = inner.err_cb.lock().as_ref() {
                match &e {
                    WincapError::HResult { component, hr, context } => cb(component, *hr, context),
                    WincapError::General { component, message } => cb(component, 0, message),
                }
            }
            if co.is_ok() {
                unsafe { CoUninitialize() };
            }
            return;
        }
    };

    let waits = [event, stop_event];
    let format = inner.format.lock().clone();

    while inner.running.load(Ordering::Acquire) {
        let r = unsafe { WaitForMultipleObjects(&waits, false, 1000) };
        if r == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            break; // stop event
        }
        if r == WAIT_TIMEOUT {
            continue;
        }
        if r != WAIT_OBJECT_0 {
            break;
        }

        loop {
            let packet = match unsafe { capture.GetNextPacketSize() } {
                Ok(p) => p,
                Err(_) => break,
            };
            if packet == 0 {
                break;
            }

            let mut data_ptr = std::ptr::null_mut::<u8>();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut _device_pos = 0u64;
            let mut qpc_pos = 0u64;

            let hr = unsafe {
                capture.GetBuffer(
                    &mut data_ptr,
                    &mut frames,
                    &mut flags,
                    Some(&mut _device_pos),
                    Some(&mut qpc_pos),
                )
            };

            if let Err(ref e) = hr {
                if e.code() == AUDCLNT_S_BUFFER_EMPTY {
                    break;
                }
            }
            if let Err(e) = hr {
                if let Some(cb) = inner.err_cb.lock().as_ref() {
                    cb("wasapi_loopback", e.code().0, "GetBuffer failed");
                }
                break;
            }

            let needed_floats = frames as usize * format.channels as usize;
            let slot_idx = match acquire_buffer(&inner.pool, needed_floats) {
                Some(idx) => idx,
                None => {
                    // Pool exhausted — drop this packet.
                    let _ = unsafe { capture.ReleaseBuffer(frames) };
                    continue;
                }
            };

            {
                let mut buf = inner.pool[slot_idx].data.lock();
                let silent = (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) != 0;
                if silent {
                    buf[..needed_floats].fill(0.0);
                } else {
                    let src = unsafe {
                        std::slice::from_raw_parts(data_ptr as *const f32, needed_floats)
                    };
                    buf[..needed_floats].copy_from_slice(src);
                }

                let discontinuity =
                    (flags & (AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32)) != 0;

                let data = buf.as_ptr();
                let inner_clone = Arc::clone(&inner);
                let chunk = AudioChunk {
                    data,
                    frame_count: frames,
                    channels: format.channels,
                    sample_rate: format.sample_rate,
                    timestamp_ns: clock::ticks_to_ns(qpc_pos),
                    silent,
                    discontinuity,
                    release: Box::new(move || {
                        inner_clone.pool[slot_idx]
                            .in_use
                            .store(0, Ordering::Release);
                    }),
                };

                if let Some(cb) = inner.cb.lock().as_ref() {
                    cb(chunk);
                } else {
                    inner.pool[slot_idx].in_use.store(0, Ordering::Release);
                }
            }

            let _ = unsafe { capture.ReleaseBuffer(frames) };
        }
    }

    let _ = unsafe { client.Stop() };
    drop(capture);
    drop(client);
    if !event.is_invalid() {
        let _ = unsafe { CloseHandle(event) };
    }
    if co.is_ok() {
        unsafe { CoUninitialize() };
    }
}

fn activate(opts: &WasapiLoopbackOptions) -> WincapResult<IAudioClient> {
    if opts.mode == LoopbackMode::SystemDefault {
        let enumerator: IMMDeviceEnumerator = hr_call!("wasapi_loopback", unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        });
        let device: IMMDevice = hr_call!("wasapi_loopback", unsafe {
            enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
        });
        let client: IAudioClient =
            hr_call!("wasapi_loopback", unsafe { device.Activate(CLSCTX_ALL, None) });
        return Ok(client);
    }

    // PROCESS_LOOPBACK (Win11 22000+)
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: opts.target_pid,
                ProcessLoopbackMode: if opts.include_tree {
                    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
                } else {
                    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
                },
            },
        },
    };

    let mut prop_var = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::default();
    // We need to set up the PROPVARIANT as VT_BLOB.
    // This is tricky with the windows crate; we'll use raw bytes.
    unsafe {
        let pv = &mut prop_var as *mut _ as *mut u8;
        // VT_BLOB = 0x41
        *(pv as *mut u16) = 0x41;
        let blob_ptr = pv.add(8); // offset to blob data in PROPVARIANT
        *(blob_ptr as *mut u32) = std::mem::size_of_val(&params) as u32; // cbSize
        *(blob_ptr.add(std::mem::size_of::<u32>()) as *mut *mut u8) =
            &mut params as *mut _ as *mut u8; // pBlobData
    }

    let done_event =
        unsafe { CreateEventW(None, true, false, None) }.map_err(|e| WincapError::HResult {
            component: "wasapi_loopback",
            hr: e.code().0,
            context: "CreateEvent for activation".into(),
        })?;

    let handler: IActivateAudioInterfaceCompletionHandler =
        ActivateHandler { done_event }.into();

    let async_op = hr_call!("wasapi_loopback", unsafe {
        ActivateAudioInterfaceAsync(
            PCWSTR(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK.as_ptr()),
            &IAudioClient::IID,
            Some(&prop_var as *const _ as *const _),
            &handler,
        )
    });

    unsafe { WaitForSingleObject(done_event, INFINITE) };
    let _ = unsafe { CloseHandle(done_event) };

    let mut activate_hr = HRESULT::default();
    let mut punk: Option<IUnknown> = None;
    hr_call!("wasapi_loopback", unsafe {
        async_op.GetActivateResult(&mut activate_hr, &mut punk)
    });
    if activate_hr.is_err() {
        return Err(WincapError::HResult {
            component: "wasapi_loopback",
            hr: activate_hr.0,
            context: "ActivateAudioInterfaceAsync result".into(),
        });
    }

    let punk = punk.ok_or_else(|| WincapError::General {
        component: "wasapi_loopback",
        message: "activation returned null".into(),
    })?;

    let client: IAudioClient = hr_call!("wasapi_loopback", punk.cast());
    Ok(client)
}

fn initialize(
    client: &IAudioClient,
    opts: &WasapiLoopbackOptions,
) -> WincapResult<(IAudioCaptureClient, HANDLE, AudioFormat)> {
    let (mix_format_ptr, owns_format) = if opts.mode == LoopbackMode::SystemDefault {
        let ptr = hr_call!("wasapi_loopback", unsafe { client.GetMixFormat() });
        (ptr, true)
    } else {
        // Process loopback: supply 48kHz float32 stereo.
        let ext = Box::new(WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: 2,
                nSamplesPerSec: 48000,
                wBitsPerSample: 32,
                nBlockAlign: 8,
                nAvgBytesPerSec: 48000 * 8,
                cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
                    - std::mem::size_of::<WAVEFORMATEX>()) as u16,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 {
                wValidBitsPerSample: 32,
            },
            dwChannelMask: SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
            SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        });
        let ptr = Box::into_raw(ext) as *mut WAVEFORMATEX;
        (ptr, false) // we own the box, not CoTaskMem
    };

    let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;

    let mut initialized = false;

    // Try IAudioClient3::InitializeSharedAudioStream for low latency.
    if opts.mode == LoopbackMode::SystemDefault {
        if let Ok(client3) = client.cast::<IAudioClient3>() {
            let mut default = 0u32;
            let mut fundamental = 0u32;
            let mut min_period = 0u32;
            let mut max_period = 0u32;
            if unsafe {
                client3.GetSharedModeEnginePeriod(
                    mix_format_ptr,
                    &mut default,
                    &mut fundamental,
                    &mut min_period,
                    &mut max_period,
                )
            }
            .is_ok()
            {
                let period = min_period.max(fundamental);
                if unsafe {
                    client3.InitializeSharedAudioStream(flags, period, mix_format_ptr, None)
                }
                .is_ok()
                {
                    initialized = true;
                }
            }
        }
    }

    if !initialized {
        // Fallback: 20ms buffer.
        const BUFFER_DURATION_HNS: i64 = 20 * 10_000;
        let hr = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                BUFFER_DURATION_HNS,
                0,
                mix_format_ptr,
                None,
            )
        };
        if let Err(e) = hr {
            if owns_format {
                unsafe { CoTaskMemFree(Some(mix_format_ptr as *const _)) };
            } else {
                let _ = unsafe { Box::from_raw(mix_format_ptr as *mut WAVEFORMATEXTENSIBLE) };
            }
            return Err(WincapError::HResult {
                component: "wasapi_loopback",
                hr: e.code().0,
                context: "IAudioClient::Initialize".into(),
            });
        }
    }

    let format = unsafe { AudioFormat::from_wave_format(mix_format_ptr) };

    if owns_format {
        unsafe { CoTaskMemFree(Some(mix_format_ptr as *const _)) };
    } else {
        let _ = unsafe { Box::from_raw(mix_format_ptr as *mut WAVEFORMATEXTENSIBLE) };
    }

    let event = unsafe { CreateEventW(None, false, false, None) }.map_err(|e| {
        WincapError::HResult {
            component: "wasapi_loopback",
            hr: e.code().0,
            context: "CreateEvent for audio".into(),
        }
    })?;

    hr_call!("wasapi_loopback", unsafe { client.SetEventHandle(event) });
    let capture: IAudioCaptureClient =
        hr_call!("wasapi_loopback", unsafe { client.GetService() });

    Ok((capture, event, format))
}
