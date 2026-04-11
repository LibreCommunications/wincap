use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi::JsFunction;
use napi_derive::napi;

use wincap_core::wasapi_loopback::{
    AudioCallback, AudioChunk, AudioErrorCallback, LoopbackMode, WasapiLoopback,
    WasapiLoopbackOptions,
};

#[napi(object)]
pub struct AudioOptionsJs {
    pub mode: String,
    pub pid: Option<u32>,
    pub include_tree: Option<bool>,
}

#[napi(object)]
pub struct AudioStatsJs {
    pub delivered_chunks: BigInt,
    pub dropped_chunks: BigInt,
    pub discontinuities: BigInt,
}

struct ChunkPayload {
    data: Vec<u8>,
    frame_count: u32,
    sample_rate: u32,
    channels: u32,
    timestamp_ns: u64,
    silent: bool,
    discontinuity: bool,
}

struct ErrorPayload {
    component: String,
    hresult: i32,
    message: String,
}

struct WasapiSend(WasapiLoopback);
unsafe impl Send for WasapiSend {}
unsafe impl Sync for WasapiSend {}
impl std::ops::Deref for WasapiSend {
    type Target = WasapiLoopback;
    fn deref(&self) -> &WasapiLoopback { &self.0 }
}
impl std::ops::DerefMut for WasapiSend {
    fn deref_mut(&mut self) -> &mut WasapiLoopback { &mut self.0 }
}

struct AudioStats {
    delivered: AtomicU64,
    dropped: AtomicU64,
    discontinuities: AtomicU64,
}

#[napi]
pub struct AudioSession {
    source: parking_lot::Mutex<WasapiSend>,
    on_chunk: ThreadsafeFunction<ChunkPayload, ErrorStrategy::Fatal>,
    on_error: ThreadsafeFunction<ErrorPayload, ErrorStrategy::Fatal>,
    running: AtomicBool,
    stats: Arc<AudioStats>,
}

#[napi]
impl AudioSession {
    #[napi(constructor)]
    pub fn new(
        _env: Env,
        opts: AudioOptionsJs,
        on_chunk: JsFunction,
        on_error: JsFunction,
    ) -> Result<Self> {
        crate::ensure_com_initialized();
        let wopts = match opts.mode.as_str() {
            "systemLoopback" => WasapiLoopbackOptions {
                mode: LoopbackMode::SystemDefault,
                ..Default::default()
            },
            "processLoopback" => {
                let pid = opts.pid
                    .ok_or_else(|| Error::from_reason("processLoopback requires opts.pid"))?;
                WasapiLoopbackOptions {
                    mode: LoopbackMode::ProcessTree,
                    target_pid: pid,
                    include_tree: opts.include_tree.unwrap_or(true),
                }
            }
            _ => return Err(Error::from_reason("mode must be 'systemLoopback' or 'processLoopback'")),
        };

        let on_chunk_tsfn = on_chunk.create_threadsafe_function(32, |ctx: ThreadSafeCallContext<ChunkPayload>| {
            let mut obj = ctx.env.create_object()?;
            obj.set("timestampNs", ctx.env.create_bigint_from_u64(ctx.value.timestamp_ns)?.into_unknown())?;
            obj.set("frameCount", ctx.value.frame_count)?;
            obj.set("sampleRate", ctx.value.sample_rate)?;
            obj.set("channels", ctx.value.channels)?;
            obj.set("format", "float32")?;
            obj.set("silent", ctx.value.silent)?;
            obj.set("discontinuity", ctx.value.discontinuity)?;
            let buf = ctx.env.create_buffer_with_data(ctx.value.data)?;
            obj.set("data", buf.into_raw())?;
            Ok(vec![obj])
        })?;

        let on_error_tsfn = on_error.create_threadsafe_function(8, |ctx: ThreadSafeCallContext<ErrorPayload>| {
            let mut obj = ctx.env.create_object()?;
            obj.set("component", ctx.value.component.as_str())?;
            obj.set("hresult", ctx.value.hresult)?;
            obj.set("message", ctx.value.message.as_str())?;
            Ok(vec![obj])
        })?;

        Ok(Self {
            source: parking_lot::Mutex::new(WasapiSend(WasapiLoopback::new(wopts))),
            on_chunk: on_chunk_tsfn,
            on_error: on_error_tsfn,
            running: AtomicBool::new(false),
            stats: Arc::new(AudioStats {
                delivered: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
                discontinuities: AtomicU64::new(0),
            }),
        })
    }

    #[napi]
    pub fn start(&self) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let on_chunk = self.on_chunk.clone();
        let on_error = self.on_error.clone();

        let stats = Arc::clone(&self.stats);

        let chunk_cb: AudioCallback = Box::new(move |chunk: AudioChunk| {
            if chunk.discontinuity {
                stats.discontinuities.fetch_add(1, Ordering::Relaxed);
            }

            let byte_count = chunk.frame_count as usize * chunk.channels as usize * 4;
            let data = unsafe {
                std::slice::from_raw_parts(chunk.data as *const u8, byte_count)
            };

            let status = on_chunk.call(
                ChunkPayload {
                    data: data.to_vec(),
                    frame_count: chunk.frame_count,
                    sample_rate: chunk.sample_rate,
                    channels: chunk.channels,
                    timestamp_ns: chunk.timestamp_ns,
                    silent: chunk.silent,
                    discontinuity: chunk.discontinuity,
                },
                ThreadsafeFunctionCallMode::NonBlocking,
            );

            match status {
                napi::Status::Ok => {
                    stats.delivered.fetch_add(1, Ordering::Relaxed);
                }
                _ => {
                    stats.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }

            // Release the pool buffer.
            (chunk.release)();
        });

        let err_cb: AudioErrorCallback =
            Box::new(move |component: &'static str, hr: i32, msg: &str| {
                let _ = on_error.call(
                    ErrorPayload {
                        component: component.to_string(),
                        hresult: hr,
                        message: msg.to_string(),
                    },
                    ThreadsafeFunctionCallMode::NonBlocking,
                );
            });

        self.source.lock().start(chunk_cb, err_cb)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(())
    }

    #[napi]
    pub fn stop(&self) -> Result<()> {
        if !self.running.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        self.source.lock().stop();
        Ok(())
    }

    #[napi]
    pub fn get_stats(&self) -> AudioStatsJs {
        AudioStatsJs {
            delivered_chunks: BigInt::from(self.stats.delivered.load(Ordering::Relaxed)),
            dropped_chunks: BigInt::from(self.stats.dropped.load(Ordering::Relaxed)),
            discontinuities: BigInt::from(self.stats.discontinuities.load(Ordering::Relaxed)),
        }
    }
}
