// Async hardware H.264 encoder via Media Foundation. Uses CLSID_MSH264EncoderMFT
// in async mode (METransformNeedInput / METransformHaveOutput events) so the
// driver schedules work without us spin-polling.
//
// Configuration emphasises low latency:
//   - eAVEncCommonRateControlMode_LowDelayVBR (or CBR if requested)
//   - AVEncCommonLowLatency = TRUE
//   - AVEncMPVDefaultBPictureCount = 0 (no B-frames)
//   - AVEncMPVGOPSize derived from keyframe_interval_ms
//   - AVEncH264CABACEnable = TRUE for compression efficiency
//
// The MFT is fed via a DXGI device manager so input NV12 textures stay
// on the GPU end-to-end.
#pragma once

#include "core/encoder/iencoder.h"

#include <atomic>
#include <mutex>
#include <queue>
#include <vector>

#include <wrl/client.h>
#include <wrl/implements.h>
#include <wrl/wrappers/corewrappers.h>

#include <mfapi.h>
#include <mfidl.h>
#include <mftransform.h>
#include <codecapi.h>
#include <icodecapi.h>

namespace wincap {

class MfEncoder final : public IEncoder {
public:
    MfEncoder();
    ~MfEncoder() override;

    void Initialize(ID3D11Device5* device, const EncoderConfig& cfg) override;
    void Start(EncodedCallback out, EncoderErrorCallback err) override;
    void Stop() override;

    void EncodeFrame(ID3D11Texture2D* surface,
                     std::uint64_t timestamp_ns,
                     const FrameOptions& opts = {}) override;
    void RequestKeyframe() override;
    void SetBitrate(std::uint32_t bps) override;

private:
    // Async callback wrapper. WRL RuntimeClass deletes operator new to
    // force Make<>(); keeping it as a separate inner class lets MfEncoder
    // itself remain a plain type usable with std::make_unique.
    class AsyncCallback
        : public Microsoft::WRL::RuntimeClass<
              Microsoft::WRL::RuntimeClassFlags<Microsoft::WRL::ClassicCom>,
              Microsoft::WRL::FtmBase,
              IMFAsyncCallback> {
    public:
        explicit AsyncCallback(MfEncoder* outer) noexcept : outer_(outer) {}
        void Detach() noexcept { outer_ = nullptr; }

        STDMETHODIMP GetParameters(DWORD* flags, DWORD* queue) override {
            if (flags) *flags = 0;
            if (queue) *queue = 0;
            return E_NOTIMPL;
        }
        STDMETHODIMP Invoke(IMFAsyncResult* result) override {
            if (outer_) outer_->Invoke(result);
            return S_OK;
        }

    private:
        MfEncoder* outer_;
    };

    void Invoke(IMFAsyncResult* result);
    void OnNeedInput();
    void OnHaveOutput();

    EncoderConfig                                   cfg_{};
    Microsoft::WRL::ComPtr<ID3D11Device5>           device_;
    Microsoft::WRL::ComPtr<IMFTransform>            mft_;
    Microsoft::WRL::ComPtr<IMFMediaEventGenerator>  event_gen_;
    Microsoft::WRL::ComPtr<ICodecAPI>               codec_api_;
    Microsoft::WRL::ComPtr<IMFDXGIDeviceManager>    dxgi_manager_;
    UINT                                            dxgi_token_{0};
    Microsoft::WRL::ComPtr<AsyncCallback>           callback_;

    EncodedCallback        on_output_;
    EncoderErrorCallback   on_error_;

    std::atomic<bool>      running_{false};
    std::atomic<uint64_t>  next_pts_hns_{0};

    // Input queue: capture thread pushes, NeedInput drains.
    struct PendingInput {
        Microsoft::WRL::ComPtr<ID3D11Texture2D> tex;
        std::uint64_t                           timestamp_ns{0};
        FrameOptions                            opts{};
        std::vector<std::int32_t>               roi_storage; // owns the rect data
    };
    std::mutex                pending_mutex_;
    std::queue<PendingInput>  pending_;

    std::atomic<bool>         force_keyframe_{false};
};

} // namespace wincap
