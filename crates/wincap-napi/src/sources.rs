use napi::bindgen_prelude::*;
use napi_derive::napi;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Threading::*;
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
    /// Numeric display index matching Electron's desktopCapturer display_id.
    pub display_id: String,
    /// Human-readable name (monitor model or "Display N (WxH)").
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
    /// Executable filename (e.g. "discord.exe").
    pub process_name: String,
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
    struct RawDisplay {
        hmon: HMONITOR,
        device_name: [u16; 32],
        bounds: RECT,
        primary: bool,
    }

    let mut raw_displays: Vec<RawDisplay> = Vec::new();

    unsafe extern "system" fn callback(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<RawDisplay>);

        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut mi.monitorInfo).as_bool() {
            out.push(RawDisplay {
                hmon,
                device_name: mi.szDevice,
                bounds: mi.monitorInfo.rcMonitor,
                primary: (mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0,
            });
        }
        TRUE
    }

    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut raw_displays as *mut _ as isize),
        );
    }

    raw_displays
        .iter()
        .enumerate()
        .map(|(idx, d)| {
            let friendly = get_friendly_display_name(&d.device_name);
            let w = d.bounds.right - d.bounds.left;
            let h = d.bounds.bottom - d.bounds.top;

            let name = if !friendly.is_empty() {
                friendly
            } else if d.primary {
                format!("Display {} ({}x{}) — Primary", idx + 1, w, h)
            } else {
                format!("Display {} ({}x{})", idx + 1, w, h)
            };

            DisplaySource {
                kind: "display".to_string(),
                monitor_handle: BigInt::from(d.hmon.0 as u64),
                display_id: idx.to_string(),
                name,
                primary: d.primary,
                bounds: Rect {
                    x: d.bounds.left,
                    y: d.bounds.top,
                    width: w,
                    height: h,
                },
            }
        })
        .collect()
}

/// Get the monitor's friendly name via DisplayConfig API (Win7+).
/// Returns the monitor model name (e.g. "DELL U2720Q", "LG ULTRAGEAR").
/// Falls back to EnumDisplayDevicesW, then empty string.
fn get_friendly_display_name(device_name: &[u16; 32]) -> String {
    // Try DisplayConfig first — most reliable for actual monitor model names.
    if let Some(name) = get_display_config_name(device_name) {
        return name;
    }

    // Fallback: EnumDisplayDevicesW
    unsafe {
        let mut dd = DISPLAY_DEVICEW::default();
        dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        if EnumDisplayDevicesW(
            windows::core::PCWSTR(device_name.as_ptr()),
            0,
            &mut dd,
            EDD_GET_DEVICE_INTERFACE_NAME,
        )
        .as_bool()
        {
            let name = wstr_to_string(&dd.DeviceString);
            if !name.is_empty()
                && !name.eq_ignore_ascii_case("Generic PnP Monitor")
                && !name.eq_ignore_ascii_case("Generic Monitor")
            {
                return name;
            }
        }
    }
    String::new()
}

/// Use QueryDisplayConfig + DisplayConfigGetDeviceInfo to get the actual
/// monitor model name. This is the same API that Windows Settings uses.
fn get_display_config_name(device_name: &[u16; 32]) -> Option<String> {
    use windows::Win32::Devices::Display::*;

    let device_str = wstr_to_string(device_name);

    unsafe {
        // Get all active paths.
        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        let flags = QDC_ONLY_ACTIVE_PATHS;
        if GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) != WIN32_ERROR(0) {
            return None;
        }

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        if QueryDisplayConfig(flags, &mut path_count, paths.as_mut_ptr(), &mut mode_count, modes.as_mut_ptr(), None) != WIN32_ERROR(0) {
            return None;
        }
        paths.truncate(path_count as usize);

        for path in &paths {
            // Get the source device name to match against our MONITORINFOEX device name.
            let mut source_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source_name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source_name.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source_name.header.adapterId = path.sourceInfo.adapterId;
            source_name.header.id = path.sourceInfo.id;

            if DisplayConfigGetDeviceInfo(&mut source_name.header) != 0i32 {
                continue;
            }

            let source_str = wstr_to_string(&source_name.viewGdiDeviceName);
            if source_str != device_str {
                continue;
            }

            // This path matches our monitor. Get the target (monitor) friendly name.
            let mut target_name = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target_name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target_name.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target_name.header.adapterId = path.targetInfo.adapterId;
            target_name.header.id = path.targetInfo.id;

            if DisplayConfigGetDeviceInfo(&mut target_name.header) != 0i32 {
                continue;
            }

            let name = wstr_to_string(&target_name.monitorFriendlyDeviceName);
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn wstr_to_string(s: &[u16]) -> String {
    let len = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..len]).trim().to_string()
}

#[napi]
pub fn list_windows() -> Vec<WindowSource> {
    let mut windows_list: Vec<WindowSource> = Vec::new();

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<WindowSource>);

        // Skip invisible windows.
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }

        // Skip windows with no title.
        if GetWindowTextLengthW(hwnd) == 0 {
            return TRUE;
        }

        // Skip tool windows.
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if (ex & WS_EX_TOOLWINDOW.0 as isize) != 0 {
            return TRUE;
        }

        // Skip windows with WS_EX_NOREDIRECTIONBITMAP (composition-only surfaces).
        if (ex & WS_EX_NOREDIRECTIONBITMAP.0 as isize) != 0 {
            return TRUE;
        }

        // Skip cloaked windows (hidden UWP system windows).
        let mut cloaked: u32 = 0;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return TRUE;
        }

        // Get title.
        let mut title_buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut title_buf);
        if n <= 0 {
            return TRUE;
        }
        let title = String::from_utf16_lossy(&title_buf[..n as usize])
            .trim()
            .to_string();
        if title.is_empty() {
            return TRUE;
        }

        // Get PID and process name.
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let process_name = get_process_name(pid);

        // Skip known system processes that produce uncapturable windows.
        let exe_lower = process_name.to_ascii_lowercase();
        if matches!(
            exe_lower.as_str(),
            "textinputhost.exe" | "shellexperiencehost.exe" | "searchhost.exe"
        ) {
            return TRUE;
        }

        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);

        // Skip zero-size windows.
        if r.right - r.left <= 0 || r.bottom - r.top <= 0 {
            return TRUE;
        }

        out.push(WindowSource {
            kind: "window".to_string(),
            hwnd: BigInt::from(hwnd.0 as u64),
            title,
            pid,
            process_name,
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
        let _ = EnumWindows(
            Some(callback),
            LPARAM(&mut windows_list as *mut _ as isize),
        );
    }
    windows_list
}

/// Get the executable filename for a PID (e.g. "discord.exe").
fn get_process_name(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
        let handle = match handle {
            Ok(h) => h,
            Err(_) => return String::new(),
        };

        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);

        if ok.is_err() || len == 0 {
            return String::new();
        }

        let full_path = String::from_utf16_lossy(&buf[..len as usize]);
        // Extract just the filename.
        full_path
            .rsplit('\\')
            .next()
            .unwrap_or(&full_path)
            .to_string()
    }
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
    type RtlGetVersionFn = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;

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
