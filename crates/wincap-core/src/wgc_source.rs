use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::*;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use crate::clock;
use crate::d3d_device::D3DDevice;
use crate::error::{hr_call, WincapError, WincapResult};
use crate::frame_pool::{FramePool, FrameSlot};

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
            }),
            item: None,
            wgc_pool: None,
            session: None,
            frame_token: None,
            closed_token: None,
        }
    }

    /// Initialise from a monitor handle (display capture).
    pub fn init_for_monitor(&mut self, monitor: HMONITOR) -> WincapResult<()> {
        let interop: IGraphicsCaptureItemInterop =
            hr_call!("wgc_source", windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>());
        let item: GraphicsCaptureItem =
            hr_call!("wgc_source", unsafe { interop.CreateForMonitor(monitor) });
        self.item = Some(item);
        Ok(())
    }

    /// Initialise from an HWND (window capture).
    pub fn init_for_window(&mut self, hwnd: HWND) -> WincapResult<()> {
        let interop: IGraphicsCaptureItemInterop =
            hr_call!("wgc_source", windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>());
        let item: GraphicsCaptureItem =
            hr_call!("wgc_source", unsafe { interop.CreateForWindow(hwnd) });
        self.item = Some(item);
        Ok(())
    }

    pub fn start(&mut self, frame_cb: FrameCallback, err_cb: ErrorCallback) -> WincapResult<()> {
        if self.inner.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        *self.inner.frame_cb.lock() = Some(frame_cb);
        *self.inner.err_cb.lock() = Some(err_cb);

        if self.item.is_none() {
            return Err(WincapError::General {
                component: "wgc_source",
                message: "capture item not initialised".into(),
            });
        }

        let size = hr_call!("wgc_source", self.item.as_ref().unwrap().Size());
        let w = size.Width as u32;
        let h = size.Height as u32;

        self.recreate_frame_pool(w, h)?;

        let item = self.item.as_ref().unwrap();
        let wgc_pool = self.wgc_pool.as_ref().unwrap();

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

        Ok(())
    }

    pub fn stop(&mut self) {
        if !self.inner.running.swap(false, Ordering::SeqCst) {
            return;
        }

        if let (Some(pool), Some(token)) = (&self.wgc_pool, self.frame_token.take()) {
            let _ = pool.RemoveFrameArrived(token);
        }
        if let (Some(item), Some(token)) = (&self.item, self.closed_token.take()) {
            let _ = item.RemoveClosed(token);
        }
        if let Some(session) = self.session.take() {
            let _ = session.Close();
        }
        if let Some(pool) = self.wgc_pool.take() {
            let _ = pool.Close();
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
                    &slot.texture,
                    0,
                    0,
                    0,
                    0,
                    &src_tex,
                    0,
                    Some(&box_),
                );
                device.context.End(&slot.fence);
            }

            let timestamp_ns = {
                let ts = hr_call!("wgc_source", frame.SystemRelativeTime());
                clock::hundred_ns_to_ns(ts.Duration)
            };

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
}

impl Drop for WgcSource {
    fn drop(&mut self) {
        self.stop();
    }
}
