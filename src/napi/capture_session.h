// JS-facing CaptureSession. Owns the D3D device, capture pool, capture
// source, and (when delivery.type === 'encoded') a VideoProcessor +
// MfEncoder pipeline that converts BGRA → NV12 and emits H.264/HEVC/AV1
// access units via a separate ThreadSafeFunction.
#pragma once

#include "core/capture/icapture_source.h"
#include "core/common/frame_pool.h"
#include "core/encoder/iencoder.h"
#include "core/gfx/d3d_device.h"
#include "core/gfx/video_processor.h"

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
    enum class DeliveryMode { Raw, Encoded };

    Napi::Value Start(const Napi::CallbackInfo& info);
    Napi::Value Stop(const Napi::CallbackInfo& info);
    Napi::Value GetStats(const Napi::CallbackInfo& info);
    Napi::Value RequestKeyframe(const Napi::CallbackInfo& info);
    Napi::Value SetBitrate(const Napi::CallbackInfo& info);

    void DispatchRawFrame(const CapturedFrame& frame);
    void DispatchEncodedFrame(const CapturedFrame& frame);
    void OnEncodedOutput(const EncodedAccessUnit& au);
    void DispatchError(const char* component, long hr, const char* msg);

    void EnsureEncoderInitialized(std::uint32_t width, std::uint32_t height);

    D3DDevice                       device_;
    FramePool                       pool_;
    std::unique_ptr<ICaptureSource> source_;

    DeliveryMode                    delivery_{DeliveryMode::Raw};

    // Encoded path.
    std::unique_ptr<VideoProcessor> color_;
    std::unique_ptr<IEncoder>       encoder_;
    std::uint32_t                   enc_width_{0};
    std::uint32_t                   enc_height_{0};
    EncoderConfig                   enc_cfg_{};

    Napi::ThreadSafeFunction        on_frame_tsfn_;
    Napi::ThreadSafeFunction        on_encoded_tsfn_;
    Napi::ThreadSafeFunction        on_error_tsfn_;

    std::atomic<bool>     running_{false};
    std::atomic<uint64_t> delivered_frames_{0};
    std::atomic<uint64_t> dropped_frames_{0};
    std::atomic<uint64_t> encoded_units_{0};
};

} // namespace wincap
