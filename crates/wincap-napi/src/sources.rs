use napi::bindgen_prelude::*;
use napi_derive::napi;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Graphics::Capture::GraphicsCaptureSession;
use windows_core::BOOL;

/// Rectangle with x, y, width, height.
#[napi(object)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[napi(object)]
pub struct DisplaySource {
    pub kind: String,
    pub monitor_handle: BigInt,
    pub name: String,
    pub primary: bool,
    pub bounds: Rect,
}

#[napi(object)]
pub struct WindowSource {
    pub kind: String,
    pub hwnd: BigInt,
    pub title: String,
    pub pid: u32,
    pub bounds: Rect,
}

#[napi(object)]
pub struct Capabilities {
    pub wgc: bool,
    pub wgc_border_optional: bool,
    pub process_loopback: bool,
    pub windows_build: u32,
}

#[napi]
pub fn list_displays() -> Vec<DisplaySource> {
    let mut displays = Vec::new();

    unsafe extern "system" fn callback(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<DisplaySource>);

        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut mi.monitorInfo).as_bool() {
            let name = String::from_utf16_lossy(
                &mi.szDevice[..mi.szDevice.iter().position(|&c| c == 0).unwrap_or(mi.szDevice.len())],
            );
            let r = mi.monitorInfo.rcMonitor;
            let primary = (mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;

            out.push(DisplaySource {
                kind: "display".to_string(),
                monitor_handle: BigInt::from(hmon.0 as u64),
                name,
                primary,
                bounds: Rect {
                    x: r.left,
                    y: r.top,
                    width: r.right - r.left,
                    height: r.bottom - r.top,
                },
            });
        }
        TRUE
    }

    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut displays as *mut _ as isize),
        );
    }
    displays
}

#[napi]
pub fn list_windows() -> Vec<WindowSource> {
    let mut windows_list: Vec<WindowSource> = Vec::new();

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<WindowSource>);

        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }
        if GetWindowTextLengthW(hwnd) == 0 {
            return TRUE;
        }

        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if (ex & WS_EX_TOOLWINDOW.0 as isize) != 0 {
            return TRUE;
        }

        let mut title = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut title);
        if n <= 0 {
            return TRUE;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);

        out.push(WindowSource {
            kind: "window".to_string(),
            hwnd: BigInt::from(hwnd.0 as u64),
            title: String::from_utf16_lossy(&title[..n as usize]),
            pid,
            bounds: Rect {
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
            },
        });
        TRUE
    }

    unsafe {
        EnumWindows(
            Some(callback),
            LPARAM(&mut windows_list as *mut _ as isize),
        )
        .ok();
    }
    windows_list
}

#[napi]
pub fn get_capabilities() -> Capabilities {
    let wgc = GraphicsCaptureSession::IsSupported().unwrap_or(false);

    let build = get_windows_build();

    Capabilities {
        wgc,
        wgc_border_optional: build >= 22621,
        process_loopback: build >= 22000,
        windows_build: build,
    }
}

fn get_windows_build() -> u32 {
    use windows::Win32::System::SystemInformation::*;
    // RtlGetVersion is the reliable way to get the build number.
    type RtlGetVersionFn =
        unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;

    unsafe {
        let ntdll = windows::Win32::System::LibraryLoader::GetModuleHandleW(
            windows::core::w!("ntdll.dll"),
        );
        if let Ok(ntdll) = ntdll {
            let proc = windows::Win32::System::LibraryLoader::GetProcAddress(
                ntdll,
                windows::core::s!("RtlGetVersion"),
            );
            if let Some(proc) = proc {
                let func: RtlGetVersionFn = std::mem::transmute(proc);
                let mut vi = OSVERSIONINFOW::default();
                vi.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
                if func(&mut vi) == 0 {
                    return vi.dwBuildNumber;
                }
            }
        }
    }
    0
}
