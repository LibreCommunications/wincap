mod sources;
mod capture_session;
mod audio_session;

use napi_derive::napi;

#[napi]
pub fn version() -> String {
    "0.1.0".to_string()
}
