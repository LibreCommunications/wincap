mod sources;
mod capture_session;
mod audio_session;

use napi_derive::napi;

#[napi]
pub fn version() -> String {
    "0.2.0".to_string()
}
