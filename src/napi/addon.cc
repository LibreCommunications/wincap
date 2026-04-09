#include "napi/capture_session.h"
#include "napi/sources.h"

#include <napi.h>

#include <combaseapi.h>
#include <winrt/base.h>

namespace wincap {

namespace {

Napi::Value Version(const Napi::CallbackInfo& info) {
    return Napi::String::New(info.Env(), "0.0.1");
}

} // namespace

Napi::Object Init(Napi::Env env, Napi::Object exports) {
    // Initialise WinRT for the JS thread once. The capture pool's
    // FrameArrived handler runs on an MTA worker that WinRT manages itself.
    static std::once_flag init_once;
    std::call_once(init_once, [] {
        winrt::init_apartment(winrt::apartment_type::multi_threaded);
    });

    exports.Set("version",         Napi::Function::New(env, Version));
    exports.Set("listDisplays",    Napi::Function::New(env, ListDisplays));
    exports.Set("listWindows",     Napi::Function::New(env, ListWindows));
    exports.Set("getCapabilities", Napi::Function::New(env, GetCapabilities));

    CaptureSession::Init(env, exports);
    return exports;
}

} // namespace wincap

NODE_API_MODULE(wincap, wincap::Init)
