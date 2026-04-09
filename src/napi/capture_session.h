// JS-facing CaptureSession. Owns the D3D device, frame pool, capture
// source, and a ThreadSafeFunction that marshals frames to the JS thread
// where they are exposed as VideoFrame objects.
#pragma once

#include "core/capture/icapture_source.h"
#include "core/common/frame_pool.h"
#include "core/gfx/d3d_device.h"

#include <atomic>
#include <memory>
#include <napi.h>

namespace wincap {

class CaptureSession : public Napi::ObjectWrap<CaptureSession> {
public:
    static Napi::Object Init(Napi::Env env, Napi::Object exports);
    explicit CaptureSession(const Napi::CallbackInfo& info);
    ~CaptureSession() override;

private:
    // JS methods.
    Napi::Value Start(const Napi::CallbackInfo& info);
    Napi::Value Stop(const Napi::CallbackInfo& info);
    Napi::Value GetStats(const Napi::CallbackInfo& info);

    void DispatchFrame(const CapturedFrame& frame);
    void DispatchError(const char* component, long hr, const char* msg);

    D3DDevice                       device_;
    FramePool                       pool_;
    std::unique_ptr<ICaptureSource> source_;

    Napi::ThreadSafeFunction        on_frame_tsfn_;
    Napi::ThreadSafeFunction        on_error_tsfn_;

    std::atomic<bool>     running_{false};
    std::atomic<uint64_t> delivered_frames_{0};
    std::atomic<uint64_t> dropped_frames_{0};
};

} // namespace wincap
