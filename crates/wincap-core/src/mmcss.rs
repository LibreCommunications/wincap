use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
    AvSetMmThreadPriority, AVRT_PRIORITY_HIGH,
};
use windows::core::PCWSTR;

/// RAII wrapper for MMCSS thread characteristics.
/// Applies the named task and HIGH priority on creation,
/// reverts on drop.
pub struct MmcssScope {
    handle: windows::Win32::Foundation::HANDLE,
}

impl MmcssScope {
    /// Create a new MMCSS scope for the given task (e.g. "Pro Audio", "Capture").
    /// Returns `None` if the API call fails.
    pub fn new(task_name: &str) -> Option<Self> {
        let wide: Vec<u16> = task_name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut task_index = 0u32;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(PCWSTR(wide.as_ptr()), &mut task_index) };
        let handle = match handle {
            Ok(h) => h,
            Err(_) => return None,
        };
        // Best-effort priority boost.
        let _ = unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_HIGH) };
        Some(Self { handle })
    }
}

impl Drop for MmcssScope {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            let _ = unsafe { AvRevertMmThreadCharacteristics(self.handle) };
        }
    }
}

// MmcssScope is not Send/Sync — it must be created and dropped on the same thread.
