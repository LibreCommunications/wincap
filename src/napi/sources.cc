#include "napi/sources.h"

#include <vector>
#include <string>

#include <windows.h>
#include <shellscalingapi.h>

#include <winrt/Windows.Graphics.Capture.h>

namespace wincap {

namespace {

struct DisplayInfo {
    HMONITOR hmon{};
    RECT     bounds{};
    std::wstring device_name;
    bool     primary{false};
};

BOOL CALLBACK MonitorEnumProc(HMONITOR hmon, HDC, LPRECT, LPARAM lparam) {
    auto* out = reinterpret_cast<std::vector<DisplayInfo>*>(lparam);
    MONITORINFOEXW mi{};
    mi.cbSize = sizeof(mi);
    if (!GetMonitorInfoW(hmon, &mi)) return TRUE;
    DisplayInfo d{};
    d.hmon        = hmon;
    d.bounds      = mi.rcMonitor;
    d.device_name = mi.szDevice;
    d.primary     = (mi.dwFlags & MONITORINFOF_PRIMARY) != 0;
    out->push_back(std::move(d));
    return TRUE;
}

std::string Narrow(const std::wstring& w) {
    if (w.empty()) return {};
    int len = WideCharToMultiByte(CP_UTF8, 0, w.data(), static_cast<int>(w.size()),
                                  nullptr, 0, nullptr, nullptr);
    std::string out(static_cast<std::size_t>(len), '\0');
    WideCharToMultiByte(CP_UTF8, 0, w.data(), static_cast<int>(w.size()),
                        out.data(), len, nullptr, nullptr);
    return out;
}

struct WindowEnumCtx {
    Napi::Env env;
    Napi::Array out;
    uint32_t idx{0};
};

BOOL CALLBACK WindowEnumProc(HWND hwnd, LPARAM lparam) {
    auto* ctx = reinterpret_cast<WindowEnumCtx*>(lparam);
    if (!IsWindowVisible(hwnd)) return TRUE;
    if (GetWindowTextLengthW(hwnd) == 0) return TRUE;

    // Skip cloaked / tool windows.
    LONG_PTR ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    if (ex & WS_EX_TOOLWINDOW) return TRUE;

    wchar_t title[512];
    int n = GetWindowTextW(hwnd, title, _countof(title));
    if (n <= 0) return TRUE;

    DWORD pid = 0;
    GetWindowThreadProcessId(hwnd, &pid);

    RECT r{};
    GetWindowRect(hwnd, &r);

    Napi::Object o = Napi::Object::New(ctx->env);
    o.Set("kind",    Napi::String::New(ctx->env, "window"));
    o.Set("hwnd",    Napi::BigInt::New(ctx->env, static_cast<uint64_t>(reinterpret_cast<uintptr_t>(hwnd))));
    o.Set("title",   Napi::String::New(ctx->env, Narrow(std::wstring(title, static_cast<std::size_t>(n)))));
    o.Set("pid",     Napi::Number::New(ctx->env, pid));
    Napi::Object bounds = Napi::Object::New(ctx->env);
    bounds.Set("x",      Napi::Number::New(ctx->env, r.left));
    bounds.Set("y",      Napi::Number::New(ctx->env, r.top));
    bounds.Set("width",  Napi::Number::New(ctx->env, r.right - r.left));
    bounds.Set("height", Napi::Number::New(ctx->env, r.bottom - r.top));
    o.Set("bounds", bounds);

    ctx->out.Set(ctx->idx++, o);
    return TRUE;
}

} // namespace

Napi::Value ListDisplays(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    std::vector<DisplayInfo> displays;
    EnumDisplayMonitors(nullptr, nullptr, &MonitorEnumProc, reinterpret_cast<LPARAM>(&displays));

    Napi::Array out = Napi::Array::New(env, displays.size());
    for (uint32_t i = 0; i < displays.size(); ++i) {
        const auto& d = displays[i];
        Napi::Object o = Napi::Object::New(env);
        o.Set("kind",          Napi::String::New(env, "display"));
        o.Set("monitorHandle", Napi::BigInt::New(env, static_cast<uint64_t>(reinterpret_cast<uintptr_t>(d.hmon))));
        o.Set("name",          Napi::String::New(env, Narrow(d.device_name)));
        o.Set("primary",       Napi::Boolean::New(env, d.primary));
        Napi::Object bounds = Napi::Object::New(env);
        bounds.Set("x",      Napi::Number::New(env, d.bounds.left));
        bounds.Set("y",      Napi::Number::New(env, d.bounds.top));
        bounds.Set("width",  Napi::Number::New(env, d.bounds.right - d.bounds.left));
        bounds.Set("height", Napi::Number::New(env, d.bounds.bottom - d.bounds.top));
        o.Set("bounds", bounds);
        out.Set(i, o);
    }
    return out;
}

Napi::Value ListWindows(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    Napi::Array out = Napi::Array::New(env);
    WindowEnumCtx ctx{env, out, 0};
    EnumWindows(&WindowEnumProc, reinterpret_cast<LPARAM>(&ctx));
    return out;
}

Napi::Value GetCapabilities(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    Napi::Object out = Napi::Object::New(env);

    bool wgc_supported = false;
    try {
        wgc_supported = winrt::Windows::Graphics::Capture::
            GraphicsCaptureSession::IsSupported();
    } catch (...) {}

    OSVERSIONINFOEXW v{};
    v.dwOSVersionInfoSize = sizeof(v);
    // RtlGetVersion is the only reliable build-number source.
    using RtlGetVersionFn = LONG (WINAPI*)(PRTL_OSVERSIONINFOW);
    DWORD build = 0;
    if (auto ntdll = GetModuleHandleW(L"ntdll.dll")) {
        if (auto fn = reinterpret_cast<RtlGetVersionFn>(GetProcAddress(ntdll, "RtlGetVersion"))) {
            if (fn(reinterpret_cast<PRTL_OSVERSIONINFOW>(&v)) == 0) {
                build = v.dwBuildNumber;
            }
        }
    }

    out.Set("wgc",                Napi::Boolean::New(env, wgc_supported));
    out.Set("wgcBorderOptional",  Napi::Boolean::New(env, build >= 22621));
    out.Set("processLoopback",    Napi::Boolean::New(env, build >= 22000));
    out.Set("windowsBuild",       Napi::Number::New(env, build));
    return out;
}

} // namespace wincap
