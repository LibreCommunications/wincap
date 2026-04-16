use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::*;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITORINFO, GetMonitorInfoW};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use crate::clock;
use crate::d3d_device::D3DDevice;
use crate::error::{hr_call, WincapError, WincapResult};
use crate::frame_pool::{FramePool, FrameSlot};

/// RAII guard that auto-releases a `FrameSlot` back to the pool on drop
/// unless explicitly disarmed. Prevents slot leaks on early `?` returns.
struct SlotGuard<'a> {
    slot: &'a FrameSlot,
    pool: &'a FramePool,
    armed: bool,
}
impl<'a> SlotGuard<'a> {
    fn new(slot: &'a FrameSlot, pool: &'a FramePool) -> Self {
        Self { slot, pool, armed: true }
    }
    fn disarm(mut self) -> &'a FrameSlot {
        self.armed = false;
        self.slot
    }
}
impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        if self.armed { self.pool.release(self.slot); }
    }
}

/// Dirty rectangle for ROI tracking.
#[derive(Debug, Clone, Copy)]
pub struct DirtyRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// A captured frame delivered to the consumer.
pub struct CapturedFrame<'a> {
    pub slot: &'a FrameSlot,
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: u64,
    pub size_changed: bool,
    pub dirty_rects: Vec<DirtyRect>,
}

pub type FrameCallback = Box<dyn Fn(CapturedFrame<'_>) + Send + Sync>;
pub type ErrorCallback = Box<dyn Fn(&'static str, i32, &str) + Send + Sync>;

pub struct WgcOptions {
    pub include_cursor: bool,
    pub border_required: bool,
    pub create_shared_handle: bool,
    pub pixel_format: DirectXPixelFormat,
}

impl Default for WgcOptions {
    fn default() -> Self {
        Self {
            include_cursor: true,
            border_required: false,
            create_shared_handle: false,
            pixel_format: DirectXPixelFormat::B8G8R8A8UIntNormalized,
        }
    }
}

/// Shared interior state for the WGC source — lives inside an `Arc` so the
/// FrameArrived / Closed event handlers (which run on WinRT MTA threads)
/// can safely access it.
struct WgcInner {
    device: *const D3DDevice,
    pool: *const FramePool,
    opts: WgcOptions,
    width: AtomicU32,
    height: AtomicU32,
    running: AtomicBool,
    // Stored as raw pointers behind Arc; we guarantee lifetime.
    frame_cb: parking_lot::Mutex<Option<FrameCallback>>,
    err_cb: parking_lot::Mutex<Option<ErrorCallback>>,
    /// Monotonically increasing counter bumped on each real WGC frame.
    frame_counter: AtomicU64,
    /// Index of the retained "idle" slot (u32::MAX = none). This slot has
    /// an extra refcount so it stays alive between real frames and can be
    /// re-submitted to the encoder when the screen is static.
    idle_slot_index: AtomicU32,
    /// Target FPS for the idle repeat timer (0 = no idle repeat).
    target_fps: AtomicU32,
}

// SAFETY: The D3DDevice and FramePool pointers are valid for the lifetime of WgcSource
// (which owns them), and WgcSource ensures Stop() is called before they're dropped.
unsafe impl Send for WgcInner {}
unsafe impl Sync for WgcInner {}

pub struct WgcSource {
    inner: Arc<WgcInner>,
    item: Option<GraphicsCaptureItem>,
    wgc_pool: Option<Direct3D11CaptureFramePool>,
    session: Option<GraphicsCaptureSession>,
    frame_token: Option<i64>,
    closed_token: Option<i64>,
    idle_thread: Option<std::thread::JoinHandle<()>>,
}

impl WgcSource {
    pub fn new(device: &D3DDevice, pool: &FramePool, opts: WgcOptions) -> Self {
        Self {
            inner: Arc::new(WgcInner {
                device: device as *const D3DDevice,
                pool: pool as *const FramePool,
                opts,
                width: AtomicU32::new(0),
                height: AtomicU32::new(0),
                running: AtomicBool::new(false),
                frame_cb: parking_lot::Mutex::new(None),
                err_cb: parking_lot::Mutex::new(None),
                frame_counter: AtomicU64::new(0),
                idle_slot_index: AtomicU32::new(u32::MAX),
                target_fps: AtomicU32::new(0),
            }),
            item: None,
            wgc_pool: None,
            session: None,
            frame_token: None,
            closed_token: None,
            idle_thread: None,
        }
    }

    /// Initialise from a monitor handle (display capture).
    pub fn init_for_monitor(&mut self, monitor: HMONITOR) -> WincapResult<()> {
        // Validate the monitor handle before calling WGC. A stale HMONITOR
        // (e.g. display disconnected between picker and capture) would
        // produce an opaque E_ACCESSDENIED from CreateForMonitor.
        if monitor.0.is_null() {
            return Err(WincapError::General {
                component: "wgc_source",
                message: "null monitor handle".into(),
            });
        }
        let mut mi = MONITORINFO::default();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if !unsafe { GetMonitorInfoW(monitor, &mut mi) }.as_bool() {
            return Err(WincapError::General {
                component: "wgc_source",
                message: format!(
                    "invalid or disconnected monitor handle ({:?}) — the display may have been unplugged",
                    monitor.0,
                ),
            });
        }

        let interop: IGraphicsCaptureItemInterop =
            hr_call!("wgc_source", windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>());
        let item: GraphicsCaptureItem =
            hr_call!("wgc_source", unsafe { interop.CreateForMonitor(monitor) });
        self.item = Some(item);
        Ok(())
    }

    /// Initialise from an HWND (window capture).
    pub fn init_for_window(&mut self, hwnd: HWND) -> WincapResult<()> {
        // Validate the window handle before calling WGC. A stale HWND
        // (e.g. window closed between picker and capture) would produce
        // an opaque E_ACCESSDENIED from CreateForWindow.
        if hwnd.0.is_null() {
            return Err(WincapError::General {
                component: "wgc_source",
                message: "null window handle".into(),
            });
        }
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err(WincapError::General {
                component: "wgc_source",
                message: format!(
                    "window handle ({:?}) is no longer valid — the window may have been closed",
                    hwnd.0,
                ),
            });
        }

        let interop: IGraphicsCaptureItemInterop =
            hr_call!("wgc_source", windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>());
        let item: GraphicsCaptureItem =
            hr_call!("wgc_source", unsafe { interop.CreateForWindow(hwnd) });
        self.item = Some(item);
        Ok(())
    }

    pub fn start(&mut self, frame_cb: FrameCallback, err_cb: ErrorCallback, fps: u32) -> WincapResult<()> {
        if self.inner.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        *self.inner.frame_cb.lock() = Some(frame_cb);
        *self.inner.err_cb.lock() = Some(err_cb);
        self.inner.target_fps.store(fps, Ordering::Release);
        self.inner.frame_counter.store(0, Ordering::Release);
        self.inner.idle_slot_index.store(u32::MAX, Ordering::Release);

        if self.item.is_none() {
            return Err(WincapError::General {
                component: "wgc_source",
                message: "capture item not initialised".into(),
            });
        }

        let item_ref = self.item.as_ref().ok_or_else(|| WincapError::General {
            component: "wgc_source",
            message: "capture item not initialised".into(),
        })?;
        let size = hr_call!("wgc_source", item_ref.Size());
        let w = size.Width as u32;
        let h = size.Height as u32;

        self.recreate_frame_pool(w, h)?;

        let item = self.item.as_ref().ok_or_else(|| WincapError::General {
            component: "wgc_source",
            message: "capture item not initialised".into(),
        })?;
        let wgc_pool = self.wgc_pool.as_ref().ok_or_else(|| WincapError::General {
            component: "wgc_source",
            message: "frame pool not created".into(),
        })?;

        // FrameArrived handler
        let inner = Arc::clone(&self.inner);
        let pool_clone = wgc_pool.clone();
        let handler = TypedEventHandler::new(
            move |_sender: windows::core::Ref<'_, Direct3D11CaptureFramePool>, _args| {
                if !inner.running.load(Ordering::Acquire) {
                    return Ok(());
                }
                Self::on_frame_arrived(&inner, &pool_clone);
                Ok(())
            },
        );
        self.frame_token = Some(hr_call!("wgc_source", wgc_pool.FrameArrived(&handler)));

        // Closed handler
        let inner2 = Arc::clone(&self.inner);
        let closed_handler = TypedEventHandler::new(
            move |_sender: windows::core::Ref<'_, GraphicsCaptureItem>, _args| {
                if let Some(cb) = inner2.err_cb.lock().as_ref() {
                    cb("wgc_source", 0, "capture item closed");
                }
                Ok(())
            },
        );
        self.closed_token = Some(hr_call!("wgc_source", item.Closed(&closed_handler)));

        let session = hr_call!("wgc_source", wgc_pool.CreateCaptureSession(item));

        // Optional features — absorb errors on older Windows builds.
        let _ = session.SetIsCursorCaptureEnabled(self.inner.opts.include_cursor);
        let _ = session.SetIsBorderRequired(self.inner.opts.border_required);

        hr_call!("wgc_source", session.StartCapture());
        self.session = Some(session);

        // Spawn idle-repeat timer thread. WGC only fires FrameArrived when
        // the compositor has new content. When the screen is static, no
        // frames arrive — this thread re-submits the last captured texture
        // to the encoder so viewers keep receiving keyframes and new
        // subscribers can decode immediately.
        if fps > 0 {
            let inner3 = Arc::clone(&self.inner);
            self.idle_thread = Some(std::thread::Builder::new()
                .name("wgc-idle-repeat".into())
                .spawn(move || Self::idle_repeat_loop(&inner3))
                .expect("failed to spawn idle-repeat thread"));
        }

        Ok(())
    }

    pub fn stop(&mut self) {
        if !self.inner.running.swap(false, Ordering::SeqCst) {
            return;
        }

        // Join idle-repeat thread before tearing down resources it references.
        if let Some(handle) = self.idle_thread.take() {
            let _ = handle.join();
        }

        // Release the retained idle slot.
        let pool = unsafe { &*self.inner.pool };
        let old_idx = self.inner.idle_slot_index.swap(u32::MAX, Ordering::AcqRel);
        if old_idx != u32::MAX {
            if let Some(slot) = pool.get_slot(old_idx) {
                pool.release(slot);
            }
        }

        if let (Some(wgc_pool), Some(token)) = (&self.wgc_pool, self.frame_token.take()) {
            let _ = wgc_pool.RemoveFrameArrived(token);
        }
        if let (Some(item), Some(token)) = (&self.item, self.closed_token.take()) {
            let _ = item.RemoveClosed(token);
        }
        if let Some(session) = self.session.take() {
            let _ = session.Close();
        }
        if let Some(wgc_pool) = self.wgc_pool.take() {
            let _ = wgc_pool.Close();
        }
        self.item = None;
        *self.inner.frame_cb.lock() = None;
        *self.inner.err_cb.lock() = None;
    }

    pub fn width(&self) -> u32 {
        self.inner.width.load(Ordering::Acquire)
    }

    pub fn height(&self) -> u32 {
        self.inner.height.load(Ordering::Acquire)
    }

    fn recreate_frame_pool(&mut self, width: u32, height: u32) -> WincapResult<()> {
        let device = unsafe { &*self.inner.device };
        let size = SizeInt32 {
            Width: width as i32,
            Height: height as i32,
        };

        if let Some(ref pool) = self.wgc_pool {
            hr_call!("wgc_source", pool.Recreate(
                device.winrt_device(),
                self.inner.opts.pixel_format,
                3,
                size,
            ));
        } else {
            let pool = hr_call!("wgc_source", Direct3D11CaptureFramePool::CreateFreeThreaded(
                device.winrt_device(),
                self.inner.opts.pixel_format,
                3,
                size,
            ));
            self.wgc_pool = Some(pool);
        }

        self.inner.width.store(width, Ordering::Release);
        self.inner.height.store(height, Ordering::Release);
        Ok(())
    }

    fn on_frame_arrived(inner: &WgcInner, wgc_pool: &Direct3D11CaptureFramePool) {
        let result = (|| -> WincapResult<()> {
            let frame = match wgc_pool.TryGetNextFrame() {
                Ok(f) => f,
                Err(_) => return Ok(()),
            };

            let content_size = hr_call!("wgc_source", frame.ContentSize());
            let w = content_size.Width as u32;
            let h = content_size.Height as u32;

            let mut size_changed = false;
            if w != inner.width.load(Ordering::Acquire)
                || h != inner.height.load(Ordering::Acquire)
            {
                size_changed = true;
                inner.width.store(w, Ordering::Release);
                inner.height.store(h, Ordering::Release);
                // Note: WGC pool recreation must happen on the owner thread.
                // We signal via size_changed and let the consumer handle it.
            }

            let pool = unsafe { &*inner.pool };
            let slot = match pool.acquire() {
                Some(s) => s,
                None => return Ok(()), // pool exhausted, drop frame
            };
            let guard = SlotGuard::new(slot, pool);

            let device = unsafe { &*inner.device };
            let surface = hr_call!("wgc_source", frame.Surface());
            let src_tex = D3DDevice::surface_to_texture(&surface)?;

            let box_ = D3D11_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: w,
                bottom: h,
                back: 1,
            };
            unsafe {
                device.context.CopySubresourceRegion(
                    &guard.slot.texture,
                    0,
                    0,
                    0,
                    0,
                    &src_tex,
                    0,
                    Some(&box_),
                );
                device.context.End(&guard.slot.fence);
            }

            let timestamp_ns = {
                let ts = hr_call!("wgc_source", frame.SystemRelativeTime());
                clock::hundred_ns_to_ns(ts.Duration)
            };

            // Disarm the guard — slot ownership transfers to the callback
            // (or we release it manually if no callback is set).
            let slot = guard.disarm();

            // Retain this slot for idle-repeat: bump refcount so it survives
            // after the callback releases its reference.
            FramePool::retain(slot);
            let old_idle = inner.idle_slot_index.swap(slot.index, Ordering::AcqRel);
            if old_idle != u32::MAX {
                if let Some(old_slot) = pool.get_slot(old_idle) {
                    pool.release(old_slot);
                }
            }
            // Signal that a real frame arrived.
            inner.frame_counter.fetch_add(1, Ordering::Release);

            let captured = CapturedFrame {
                slot,
                width: w,
                height: h,
                timestamp_ns,
                size_changed,
                dirty_rects: Vec::new(),
            };

            if let Some(cb) = inner.frame_cb.lock().as_ref() {
                cb(captured);
            } else {
                pool.release(slot);
            }

            Ok(())
        })();

        if let Err(e) = result {
            if let Some(cb) = inner.err_cb.lock().as_ref() {
                match &e {
                    WincapError::HResult {
                        component, hr, context,
                    } => cb(component, *hr, context),
                    WincapError::General { component, message } => cb(component, 0, message),
                }
            }
        }
    }

    /// Background thread that re-submits the last captured frame to the
    /// encoder when WGC goes idle (no FrameArrived events). This ensures
    /// the encoder keeps producing periodic keyframes so late joiners can
    /// decode immediately and WebRTC congestion control stays healthy.
    fn idle_repeat_loop(inner: &WgcInner) {
        let fps = inner.target_fps.load(Ordering::Acquire);
        if fps == 0 { return; }

        // Sleep for 2x the frame interval before considering the source idle.
        let interval = std::time::Duration::from_millis((1000 / fps as u64).max(1));
        let idle_threshold = interval * 2;

        let mut last_seen_counter = 0u64;
        let mut idle_since: Option<std::time::Instant> = None;

        while inner.running.load(Ordering::Acquire) {
            std::thread::sleep(interval);

            if !inner.running.load(Ordering::Acquire) {
                break;
            }

            let counter = inner.frame_counter.load(Ordering::Acquire);

            if counter != last_seen_counter {
                // A real frame arrived — reset idle tracking.
                last_seen_counter = counter;
                idle_since = None;
                continue;
            }

            // No new frame. Track how long we've been idle.
            let now = std::time::Instant::now();
            let idle_start = *idle_since.get_or_insert(now);

            if now.duration_since(idle_start) < idle_threshold {
                continue;
            }

            // Screen is idle — re-submit the last captured texture.
            let slot_idx = inner.idle_slot_index.load(Ordering::Acquire);
            if slot_idx == u32::MAX {
                continue; // No frame captured yet.
            }

            let pool = unsafe { &*inner.pool };
            let slot = match pool.get_slot(slot_idx) {
                Some(s) => s,
                None => continue,
            };

            // Retain for this callback invocation. The callback will release
            // one ref; the idle retention (from on_frame_arrived) persists.
            FramePool::retain(slot);

            let w = inner.width.load(Ordering::Acquire);
            let h = inner.height.load(Ordering::Acquire);

            let captured = CapturedFrame {
                slot,
                width: w,
                height: h,
                timestamp_ns: clock::now_ns(),
                size_changed: false,
                dirty_rects: Vec::new(),
            };

            if let Some(cb) = inner.frame_cb.lock().as_ref() {
                cb(captured);
            } else {
                pool.release(slot);
            }
        }
    }
}

impl Drop for WgcSource {
    fn drop(&mut self) {
        self.stop();
    }
}
