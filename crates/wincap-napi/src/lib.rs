mod sources;
mod capture_session;
mod audio_session;

use napi_derive::napi;
use std::sync::Once;

/// Initialise COM for the JS thread once. In Electron the main thread
/// is already STA; we accept RPC_E_CHANGED_MODE silently.
pub(crate) fn ensure_com_initialized() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        unsafe {
            let hr = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            );
            // 0 = S_OK, 1 = S_FALSE (already init), 0x80010106 = RPC_E_CHANGED_MODE (STA)
            let code = hr.0 as u32;
            if code != 0 && code != 1 && code != 0x80010106 {
                eprintln!("wincap: CoInitializeEx failed: {:#010X}", code);
            }
        }
    });
}

#[napi]
pub fn version() -> String {
    ensure_com_initialized();
    "0.3.1".to_string()
}
