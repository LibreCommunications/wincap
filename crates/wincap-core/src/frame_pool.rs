use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Foundation::HANDLE;
use windows::core::Interface;

use crate::error::{hr_call, WincapError, WincapResult};

pub const MAX_SLOTS: u32 = 8;

pub struct FrameSlot {
    pub texture: ID3D11Texture2D,
    pub fence: ID3D11Query,
    pub shared_nt: Option<HANDLE>,
    pub index: u32,
    pub refcount: AtomicU32,
}

// SAFETY: The COM objects are thread-safe (D3D11 multithread-protected context).
// The atomic refcount is inherently thread-safe.
unsafe impl Send for FrameSlot {}
unsafe impl Sync for FrameSlot {}

pub struct FramePool {
    slots: Vec<FrameSlot>,
    free_mask: AtomicU32,
}

// SAFETY: FramePool is designed for one producer + one consumer with atomic free_mask.
unsafe impl Send for FramePool {}
unsafe impl Sync for FramePool {}

impl FramePool {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_mask: AtomicU32::new(0),
        }
    }

    /// Allocate `count` slots of the given description.
    pub fn init(
        &mut self,
        device: &ID3D11Device5,
        count: u32,
        mut desc: D3D11_TEXTURE2D_DESC,
        create_shared_handle: bool,
    ) -> WincapResult<()> {
        if count == 0 || count > MAX_SLOTS {
            return Err(WincapError::General {
                component: "frame_pool",
                message: format!("count must be 1..{MAX_SLOTS}, got {count}"),
            });
        }

        if create_shared_handle {
            desc.MiscFlags |= D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 as u32
                | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32;
        }

        self.slots.clear();

        for i in 0..count {
            let mut texture: Option<ID3D11Texture2D> = None;
            hr_call!("frame_pool", unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) });
            let texture = texture.ok_or_else(|| crate::error::WincapError::General {
                component: "frame_pool",
                message: "CreateTexture2D returned None".into(),
            })?;

            let query_desc = D3D11_QUERY_DESC {
                Query: D3D11_QUERY_EVENT,
                MiscFlags: 0,
            };
            let mut fence: Option<ID3D11Query> = None;
            hr_call!("frame_pool", unsafe { device.CreateQuery(&query_desc, Some(&mut fence)) });
            let fence = fence.ok_or_else(|| crate::error::WincapError::General {
                component: "frame_pool",
                message: "CreateQuery returned None".into(),
            })?;

            let shared_nt = if create_shared_handle {
                let dxgi_res: IDXGIResource1 = hr_call!("frame_pool", texture.cast());
                let handle = hr_call!("frame_pool", unsafe {
                    dxgi_res.CreateSharedHandle(
                        None,
                        (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0,
                        None,
                    )
                });
                Some(handle)
            } else {
                None
            };

            self.slots.push(FrameSlot {
                texture,
                fence,
                shared_nt,
                index: i,
                refcount: AtomicU32::new(0),
            });
        }

        // Mark all slots free.
        self.free_mask
            .store((1u32 << count) - 1, Ordering::Release);
        Ok(())
    }

    pub fn shutdown(&mut self) {
        for slot in &mut self.slots {
            if let Some(handle) = slot.shared_nt.take() {
                let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
            }
        }
        self.slots.clear();
        self.free_mask.store(0, Ordering::Release);
    }

    /// Producer: acquire a free slot (refcount=1). Returns `None` if pool exhausted.
    pub fn acquire(&self) -> Option<&FrameSlot> {
        let mut mask = self.free_mask.load(Ordering::Acquire);
        while mask != 0 {
            let bit = mask.trailing_zeros();
            let want = mask & !(1u32 << bit);
            match self.free_mask.compare_exchange_weak(
                mask,
                want,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let slot = &self.slots[bit as usize];
                    slot.refcount.store(1, Ordering::Release);
                    return Some(slot);
                }
                Err(updated) => mask = updated,
            }
        }
        None
    }

    /// Increment refcount for a slot being shared with JS.
    pub fn retain(slot: &FrameSlot) {
        slot.refcount.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrement refcount; when it hits 0 the slot returns to the free list.
    pub fn release(&self, slot: &FrameSlot) {
        if slot.refcount.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.free_mask
                .fetch_or(1u32 << slot.index, Ordering::Release);
        }
    }

    pub fn capacity(&self) -> u32 {
        self.slots.len() as u32
    }

    pub fn get_slot(&self, index: u32) -> Option<&FrameSlot> {
        self.slots.get(index as usize)
    }
}

impl Drop for FramePool {
    fn drop(&mut self) {
        self.shutdown();
    }
}
