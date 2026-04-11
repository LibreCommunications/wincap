use std::sync::OnceLock;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

static FREQUENCY: OnceLock<u64> = OnceLock::new();

fn frequency() -> u64 {
    *FREQUENCY.get_or_init(|| {
        let mut freq = 0i64;
        unsafe { QueryPerformanceFrequency(&mut freq).unwrap() };
        freq as u64
    })
}

/// Initialise the global clock. Idempotent, thread-safe.
pub fn init() {
    let _ = frequency();
}

/// Current QPC counter value.
pub fn now_ticks() -> u64 {
    let mut ticks = 0i64;
    unsafe { QueryPerformanceCounter(&mut ticks).unwrap() };
    ticks as u64
}

/// Convert raw QPC ticks to nanoseconds. Splits to avoid 64-bit overflow.
pub fn ticks_to_ns(ticks: u64) -> u64 {
    let freq = frequency();
    let whole = ticks / freq;
    let rem = ticks % freq;
    whole * 1_000_000_000 + (rem * 1_000_000_000) / freq
}

/// Current time in nanoseconds since the QPC epoch.
pub fn now_ns() -> u64 {
    ticks_to_ns(now_ticks())
}

/// Convert a WinRT TimeSpan (100-ns units) to nanoseconds.
pub fn hundred_ns_to_ns(hns: i64) -> u64 {
    (hns as u64) * 100
}
