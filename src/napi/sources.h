// Source enumeration: list displays / list windows. Picker (Win11
// GraphicsCapturePicker) lands in M3.
#pragma once

#include <napi.h>

namespace wincap {

Napi::Value ListDisplays(const Napi::CallbackInfo& info);
Napi::Value ListWindows(const Napi::CallbackInfo& info);
Napi::Value GetCapabilities(const Napi::CallbackInfo& info);

} // namespace wincap
