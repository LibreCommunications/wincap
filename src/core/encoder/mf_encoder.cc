#include "core/encoder/mf_encoder.h"

#include "core/common/errors.h"

#include <mferror.h>
#include <wmcodecdsp.h>

#pragma comment(lib, "mfplat.lib")
#pragma comment(lib, "mfuuid.lib")
#pragma comment(lib, "mf.lib")
#pragma comment(lib, "wmcodecdspuuid.lib")

namespace wincap {

namespace {

constexpr GUID kMfvCodecH264 = MFVideoFormat_H264;
constexpr GUID kMfvCodecHEVC = MFVideoFormat_HEVC;

void SetUInt32(ICodecAPI* api, REFGUID prop, ULONG v) {
    VARIANT var{};
    var.vt = VT_UI4;
    var.ulVal = v;
    api->SetValue(&prop, &var);
}

void SetBool(ICodecAPI* api, REFGUID prop, bool b) {
    VARIANT var{};
    var.vt = VT_BOOL;
    var.boolVal = b ? VARIANT_TRUE : VARIANT_FALSE;
    api->SetValue(&prop, &var);
}

} // namespace

MfEncoder::MfEncoder() {
    MFStartup(MF_VERSION, MFSTARTUP_FULL);
}

MfEncoder::~MfEncoder() {
    Stop();
    MFShutdown();
}

void MfEncoder::Initialize(ID3D11Device5* device, const EncoderConfig& cfg) {
    cfg_    = cfg;
    device_ = device;

    // 1. Wrap the D3D11 device in an MFDXGIDeviceManager so the encoder
    //    accepts D3D11-resident NV12 textures as input.
    WINCAP_THROW_IF_FAILED("mf_encoder",
        MFCreateDXGIDeviceManager(&dxgi_token_, dxgi_manager_.GetAddressOf()));
    WINCAP_THROW_IF_FAILED("mf_encoder",
        dxgi_manager_->ResetDevice(device_.Get(), dxgi_token_));

    // 2. Locate a hardware async MFT for the requested codec.
    GUID subtype = kMfvCodecH264;
    if (cfg.codec == VideoCodec::HEVC) subtype = kMfvCodecHEVC;
    if (cfg.codec == VideoCodec::AV1)  subtype = MFVideoFormat_AV1;

    MFT_REGISTER_TYPE_INFO out_info{ MFMediaType_Video, subtype };
    UINT32 flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT |
                   MFT_ENUM_FLAG_SORTANDFILTER;

    IMFActivate** activates = nullptr;
    UINT32 count = 0;
    WINCAP_THROW_IF_FAILED("mf_encoder",
        MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, nullptr, &out_info,
                  &activates, &count));
    if (count == 0) {
        if (activates) CoTaskMemFree(activates);
        throw HrError(E_NOTIMPL, "mf_encoder", "no hardware async encoder available");
    }

    HRESULT activate_hr = activates[0]->ActivateObject(IID_PPV_ARGS(mft_.GetAddressOf()));
    for (UINT32 i = 0; i < count; ++i) activates[i]->Release();
    CoTaskMemFree(activates);
    WINCAP_THROW_IF_FAILED("mf_encoder", activate_hr);

    // 3. Bind the DXGI manager so the MFT uses our D3D device.
    WINCAP_THROW_IF_FAILED("mf_encoder",
        mft_->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER,
                             reinterpret_cast<ULONG_PTR>(dxgi_manager_.Get())));

    // 4. Async unlock — required before setting media types.
    Microsoft::WRL::ComPtr<IMFAttributes> attrs;
    WINCAP_THROW_IF_FAILED("mf_encoder", mft_->GetAttributes(attrs.GetAddressOf()));
    attrs->SetUINT32(MF_TRANSFORM_ASYNC_UNLOCK, TRUE);
    attrs->SetUINT32(MF_LOW_LATENCY, TRUE);

    // 5. Output media type (must be set before input).
    Microsoft::WRL::ComPtr<IMFMediaType> out_type;
    WINCAP_THROW_IF_FAILED("mf_encoder", MFCreateMediaType(out_type.GetAddressOf()));
    out_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    out_type->SetGUID(MF_MT_SUBTYPE,    subtype);
    out_type->SetUINT32(MF_MT_AVG_BITRATE, cfg.bitrate_bps);
    MFSetAttributeSize(out_type.Get(), MF_MT_FRAME_SIZE, cfg.width, cfg.height);
    MFSetAttributeRatio(out_type.Get(), MF_MT_FRAME_RATE, cfg.fps, 1);
    MFSetAttributeRatio(out_type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1);
    out_type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
    if (cfg.codec == VideoCodec::H264) {
        out_type->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High);
    }
    WINCAP_THROW_IF_FAILED("mf_encoder", mft_->SetOutputType(0, out_type.Get(), 0));

    // 6. Input media type — NV12 from D3D11.
    Microsoft::WRL::ComPtr<IMFMediaType> in_type;
    WINCAP_THROW_IF_FAILED("mf_encoder", MFCreateMediaType(in_type.GetAddressOf()));
    in_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    in_type->SetGUID(MF_MT_SUBTYPE,    MFVideoFormat_NV12);
    MFSetAttributeSize(in_type.Get(), MF_MT_FRAME_SIZE, cfg.width, cfg.height);
    MFSetAttributeRatio(in_type.Get(), MF_MT_FRAME_RATE, cfg.fps, 1);
    MFSetAttributeRatio(in_type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1);
    in_type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
    WINCAP_THROW_IF_FAILED("mf_encoder", mft_->SetInputType(0, in_type.Get(), 0));

    // 7. ICodecAPI tuning. All best-effort — old drivers reject some props.
    if (SUCCEEDED(mft_.As(&codec_api_))) {
        SetUInt32(codec_api_.Get(), CODECAPI_AVEncCommonRateControlMode,
                  eAVEncCommonRateControlMode_LowDelayVBR);
        SetUInt32(codec_api_.Get(), CODECAPI_AVEncCommonMeanBitRate, cfg.bitrate_bps);
        SetUInt32(codec_api_.Get(), CODECAPI_AVEncMPVDefaultBPictureCount, 0);
        SetUInt32(codec_api_.Get(), CODECAPI_AVEncMPVGOPSize,
                  std::max<UINT32>(1, cfg.fps * cfg.keyframe_interval_ms / 1000));
        SetBool(codec_api_.Get(), CODECAPI_AVLowLatencyMode, true);
        SetBool(codec_api_.Get(), CODECAPI_AVEncCommonRealTime, true);
        if (cfg.codec == VideoCodec::H264) {
            SetBool(codec_api_.Get(), CODECAPI_AVEncH264CABACEnable, true);
        }
    }

    // 8. Grab the event generator for async event delivery.
    WINCAP_THROW_IF_FAILED("mf_encoder", mft_.As(&event_gen_));
}

void MfEncoder::Start(EncodedCallback out, EncoderErrorCallback err) {
    if (running_.exchange(true)) return;
    on_output_ = std::move(out);
    on_error_  = std::move(err);

    WINCAP_THROW_IF_FAILED("mf_encoder",
        mft_->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0));
    WINCAP_THROW_IF_FAILED("mf_encoder",
        mft_->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0));

    // Subscribe to the first event; each Invoke() re-subscribes.
    WINCAP_THROW_IF_FAILED("mf_encoder",
        event_gen_->BeginGetEvent(this, nullptr));
}

void MfEncoder::Stop() {
    if (!running_.exchange(false)) return;
    if (mft_) {
        mft_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        mft_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        mft_->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
    }
    on_output_ = nullptr;
    on_error_  = nullptr;
    std::lock_guard<std::mutex> lk(pending_mutex_);
    while (!pending_.empty()) pending_.pop();
}

void MfEncoder::EncodeFrame(ID3D11Texture2D* nv12, std::uint64_t timestamp_ns) {
    if (!running_.load(std::memory_order_acquire)) return;
    PendingInput in{};
    in.tex          = nv12;
    in.timestamp_ns = timestamp_ns;
    {
        std::lock_guard<std::mutex> lk(pending_mutex_);
        pending_.push(std::move(in));
    }
}

void MfEncoder::RequestKeyframe() { force_keyframe_.store(true, std::memory_order_release); }

void MfEncoder::SetBitrate(std::uint32_t bps) {
    if (!codec_api_) return;
    SetUInt32(codec_api_.Get(), CODECAPI_AVEncCommonMeanBitRate, bps);
}

STDMETHODIMP MfEncoder::GetParameters(DWORD* flags, DWORD* queue) {
    if (flags) *flags = 0;
    if (queue) *queue = 0;
    return E_NOTIMPL; // use defaults
}

STDMETHODIMP MfEncoder::Invoke(IMFAsyncResult* result) {
    if (!running_.load(std::memory_order_acquire) || !event_gen_) return S_OK;

    Microsoft::WRL::ComPtr<IMFMediaEvent> evt;
    HRESULT hr = event_gen_->EndGetEvent(result, evt.GetAddressOf());
    if (FAILED(hr)) {
        if (on_error_) on_error_("mf_encoder", hr, "EndGetEvent failed");
        return S_OK;
    }

    MediaEventType type = MEUnknown;
    evt->GetType(&type);

    try {
        if (type == METransformNeedInput) {
            OnNeedInput();
        } else if (type == METransformHaveOutput) {
            OnHaveOutput();
        }
    } catch (HrError const& e) {
        if (on_error_) on_error_(e.component(), e.hr(), e.what());
    }

    if (running_.load(std::memory_order_acquire)) {
        event_gen_->BeginGetEvent(this, nullptr);
    }
    return S_OK;
}

void MfEncoder::OnNeedInput() {
    PendingInput in;
    {
        std::lock_guard<std::mutex> lk(pending_mutex_);
        if (pending_.empty()) return;
        in = std::move(pending_.front());
        pending_.pop();
    }

    Microsoft::WRL::ComPtr<IMFSample> sample;
    WINCAP_THROW_IF_FAILED("mf_encoder", MFCreateSample(sample.GetAddressOf()));
    Microsoft::WRL::ComPtr<IMFMediaBuffer> buf;
    WINCAP_THROW_IF_FAILED("mf_encoder",
        MFCreateDXGISurfaceBuffer(IID_ID3D11Texture2D, in.tex.Get(), 0, FALSE,
                                  buf.GetAddressOf()));
    WINCAP_THROW_IF_FAILED("mf_encoder", sample->AddBuffer(buf.Get()));

    // PTS in 100-ns units.
    const LONGLONG pts_hns = static_cast<LONGLONG>(in.timestamp_ns / 100ull);
    sample->SetSampleTime(pts_hns);
    sample->SetSampleDuration(10'000'000ll / std::max<UINT32>(1, cfg_.fps));

    if (force_keyframe_.exchange(false, std::memory_order_acq_rel)) {
        sample->SetUINT32(MFSampleExtension_CleanPoint, TRUE);
    }

    WINCAP_THROW_IF_FAILED("mf_encoder", mft_->ProcessInput(0, sample.Get(), 0));
}

void MfEncoder::OnHaveOutput() {
    MFT_OUTPUT_STREAM_INFO info{};
    WINCAP_THROW_IF_FAILED("mf_encoder", mft_->GetOutputStreamInfo(0, &info));

    MFT_OUTPUT_DATA_BUFFER out{};
    DWORD status = 0;

    Microsoft::WRL::ComPtr<IMFSample> sample;
    if (!(info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
        WINCAP_THROW_IF_FAILED("mf_encoder", MFCreateSample(sample.GetAddressOf()));
        Microsoft::WRL::ComPtr<IMFMediaBuffer> buf;
        WINCAP_THROW_IF_FAILED("mf_encoder",
            MFCreateMemoryBuffer(info.cbSize, buf.GetAddressOf()));
        sample->AddBuffer(buf.Get());
        out.pSample = sample.Get();
    }

    HRESULT hr = mft_->ProcessOutput(0, 1, &out, &status);
    if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) return;
    if (FAILED(hr)) {
        if (out.pEvents) out.pEvents->Release();
        WINCAP_THROW_IF_FAILED("mf_encoder", hr);
    }

    Microsoft::WRL::ComPtr<IMFSample> got_sample;
    got_sample.Attach(out.pSample);
    if (out.pEvents) out.pEvents->Release();

    LONGLONG pts_hns = 0;
    got_sample->GetSampleTime(&pts_hns);

    UINT32 keyframe = 0;
    got_sample->GetUINT32(MFSampleExtension_CleanPoint, &keyframe);

    Microsoft::WRL::ComPtr<IMFMediaBuffer> buf;
    WINCAP_THROW_IF_FAILED("mf_encoder", got_sample->ConvertToContiguousBuffer(buf.GetAddressOf()));

    BYTE*  data = nullptr;
    DWORD  cur  = 0;
    DWORD  max  = 0;
    WINCAP_THROW_IF_FAILED("mf_encoder", buf->Lock(&data, &max, &cur));

    if (on_output_) {
        EncodedAccessUnit au{};
        au.data         = data;
        au.size         = cur;
        au.timestamp_ns = static_cast<std::uint64_t>(pts_hns) * 100ull;
        au.keyframe     = keyframe != 0;
        on_output_(au);
    }

    buf->Unlock();
}

void MfEncoder::DrainOutputUnsafe() { /* unused — async path drains via events */ }

} // namespace wincap
