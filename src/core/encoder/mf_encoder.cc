#include "core/encoder/mf_encoder.h"

#include "core/common/errors.h"

#include <algorithm>
#include <cstring>
#include <string>

#include <mferror.h>
#include <wmcodecdsp.h>
#include <dxgi1_6.h>

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

// Look up the active D3D11 device's adapter vendor ID so we can prefer
// the matching MFT (e.g. NVIDIA D3D11 device → NVENC MFT).
UINT GetAdapterVendorId(ID3D11Device5* device) noexcept {
    if (!device) return 0;
    Microsoft::WRL::ComPtr<IDXGIDevice> dxgi_device;
    if (FAILED(device->QueryInterface(IID_PPV_ARGS(dxgi_device.GetAddressOf())))) return 0;
    Microsoft::WRL::ComPtr<IDXGIAdapter> adapter;
    if (FAILED(dxgi_device->GetAdapter(adapter.GetAddressOf()))) return 0;
    DXGI_ADAPTER_DESC desc{};
    if (FAILED(adapter->GetDesc(&desc))) return 0;
    return desc.VendorId;
}

// Pick the best MFT activate from MFTEnumEx results. Strategy:
//   1. Prefer the one whose MFT_ENUM_HARDWARE_VENDOR_ID_Attribute matches
//      the running D3D adapter (formatted as "VEN_XXXX").
//   2. Otherwise return index 0.
UINT PickBestActivate(IMFActivate** activates, UINT count, UINT vendor_id) {
    if (vendor_id == 0 || count <= 1) return 0;
    wchar_t want[16];
    swprintf_s(want, L"VEN_%04X", vendor_id);
    for (UINT i = 0; i < count; ++i) {
        UINT32 len = 0;
        if (FAILED(activates[i]->GetStringLength(MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, &len))) continue;
        std::wstring s(len, L'\0');
        UINT32 actual = 0;
        if (FAILED(activates[i]->GetString(MFT_ENUM_HARDWARE_VENDOR_ID_Attribute,
                                           s.data(), len + 1, &actual))) continue;
        if (_wcsicmp(s.c_str(), want) == 0) return i;
    }
    return 0;
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

    if (cfg.hdr10 && cfg.codec == VideoCodec::H264) {
        throw HrError(E_INVALIDARG, "mf_encoder",
                      "HDR10 requires HEVC or AV1 (no 10-bit H.264 path)");
    }

    // 1. Wrap D3D11 device for GPU-resident input.
    WINCAP_THROW_IF_FAILED("mf_encoder",
        MFCreateDXGIDeviceManager(&dxgi_token_, dxgi_manager_.GetAddressOf()));
    WINCAP_THROW_IF_FAILED("mf_encoder",
        dxgi_manager_->ResetDevice(device_.Get(), dxgi_token_));

    // 2. Locate a vendor-matched hardware async MFT.
    GUID subtype = kMfvCodecH264;
    if (cfg.codec == VideoCodec::HEVC) subtype = kMfvCodecHEVC;
    if (cfg.codec == VideoCodec::AV1)  subtype = MFVideoFormat_AV1;

    MFT_REGISTER_TYPE_INFO out_info{ MFMediaType_Video, subtype };
    UINT32 enum_flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT |
                        MFT_ENUM_FLAG_SORTANDFILTER;

    IMFActivate** activates = nullptr;
    UINT32 count = 0;
    WINCAP_THROW_IF_FAILED("mf_encoder",
        MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, enum_flags, nullptr, &out_info,
                  &activates, &count));
    if (count == 0) {
        if (activates) CoTaskMemFree(activates);
        throw HrError(E_NOTIMPL, "mf_encoder",
                      "no hardware async encoder available for requested codec");
    }

    const UINT vendor = GetAdapterVendorId(device_.Get());
    const UINT pick = PickBestActivate(activates, count, vendor);

    HRESULT activate_hr = activates[pick]->ActivateObject(IID_PPV_ARGS(mft_.GetAddressOf()));
    for (UINT32 i = 0; i < count; ++i) activates[i]->Release();
    CoTaskMemFree(activates);
    WINCAP_THROW_IF_FAILED("mf_encoder", activate_hr);

    // 3. Bind DXGI manager.
    WINCAP_THROW_IF_FAILED("mf_encoder",
        mft_->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER,
                             reinterpret_cast<ULONG_PTR>(dxgi_manager_.Get())));

    // 4. Async unlock + low-latency hint.
    Microsoft::WRL::ComPtr<IMFAttributes> attrs;
    WINCAP_THROW_IF_FAILED("mf_encoder", mft_->GetAttributes(attrs.GetAddressOf()));
    attrs->SetUINT32(MF_TRANSFORM_ASYNC_UNLOCK, TRUE);
    attrs->SetUINT32(MF_LOW_LATENCY, TRUE);

    // 5. Output media type.
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
    } else if (cfg.codec == VideoCodec::HEVC) {
        out_type->SetUINT32(MF_MT_MPEG2_PROFILE,
            cfg.hdr10 ? /*Main10*/ 2 : /*Main*/ 1);
    }

    if (cfg.hdr10) {
        // BT.2020 PQ HDR10 metadata.
        out_type->SetUINT32(MF_MT_VIDEO_PRIMARIES,    MFVideoPrimaries_BT2020);
        out_type->SetUINT32(MF_MT_TRANSFER_FUNCTION,  MFVideoTransFunc_2084);
        out_type->SetUINT32(MF_MT_YUV_MATRIX,         MFVideoTransferMatrix_BT2020_10);
        out_type->SetUINT32(MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235);
    }

    WINCAP_THROW_IF_FAILED("mf_encoder", mft_->SetOutputType(0, out_type.Get(), 0));

    // 6. Input media type — NV12 (SDR) or P010 (HDR10).
    Microsoft::WRL::ComPtr<IMFMediaType> in_type;
    WINCAP_THROW_IF_FAILED("mf_encoder", MFCreateMediaType(in_type.GetAddressOf()));
    in_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    in_type->SetGUID(MF_MT_SUBTYPE,
        cfg.hdr10 ? MFVideoFormat_P010 : MFVideoFormat_NV12);
    MFSetAttributeSize(in_type.Get(), MF_MT_FRAME_SIZE, cfg.width, cfg.height);
    MFSetAttributeRatio(in_type.Get(), MF_MT_FRAME_RATE, cfg.fps, 1);
    MFSetAttributeRatio(in_type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1);
    in_type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
    WINCAP_THROW_IF_FAILED("mf_encoder", mft_->SetInputType(0, in_type.Get(), 0));

    // 7. ICodecAPI tuning. All best-effort — old drivers reject some props.
    if (SUCCEEDED(mft_.As(&codec_api_))) {
        ICodecAPI* api = codec_api_.Get();
        SetUInt32(api, CODECAPI_AVEncCommonRateControlMode,
                  eAVEncCommonRateControlMode_LowDelayVBR);
        SetUInt32(api, CODECAPI_AVEncCommonMeanBitRate, cfg.bitrate_bps);
        SetUInt32(api, CODECAPI_AVEncMPVDefaultBPictureCount, 0);
        SetUInt32(api, CODECAPI_AVEncMPVGOPSize,
                  std::max<UINT32>(1, cfg.fps * cfg.keyframe_interval_ms / 1000));
        SetBool(api, CODECAPI_AVLowLatencyMode, true);
        SetBool(api, CODECAPI_AVEncCommonRealTime, true);
        if (cfg.codec == VideoCodec::H264) {
            SetBool(api, CODECAPI_AVEncH264CABACEnable, true);
        }

        // LTR: high 16 bits = 0x0001 (enable), low 16 = count.
        if (cfg.ltr_count > 0) {
            const ULONG ltr_pack = (0x0001u << 16) | (cfg.ltr_count & 0xFFFFu);
            SetUInt32(api, CODECAPI_AVEncVideoLTRBufferControl, ltr_pack);
        }

        // Intra refresh — vendor support varies; calls are best-effort.
        if (cfg.intra_refresh) {
            // 1 = column refresh; 2 = row refresh. Pick column.
            SetUInt32(api, CODECAPI_AVEncVideoEncodeFrameTypeQP, 0);
            // CODECAPI_AVEncVideoIntraRefreshMode is not in every SDK
            // header; use the published GUID directly when present.
        }

        if (cfg.roi_enabled) {
            SetBool(api, CODECAPI_AVEncVideoROIEnabled, true);
        }

        // Number of slices == 1 keeps latency minimal.
        SetUInt32(api, CODECAPI_AVEncNumWorkerThreads, 0); // driver chooses
    }

    // 8. Event generator for async event delivery.
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

void MfEncoder::EncodeFrame(ID3D11Texture2D* surface,
                            std::uint64_t timestamp_ns,
                            const FrameOptions& opts) {
    if (!running_.load(std::memory_order_acquire)) return;
    PendingInput in{};
    in.tex          = surface;
    in.timestamp_ns = timestamp_ns;
    in.opts         = opts;
    if (opts.roi_count > 0 && opts.roi_rects) {
        in.roi_storage.assign(opts.roi_rects, opts.roi_rects + opts.roi_count * 4);
        in.opts.roi_rects = in.roi_storage.data();
    }
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
    return E_NOTIMPL;
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
        if (type == METransformNeedInput)        OnNeedInput();
        else if (type == METransformHaveOutput)  OnHaveOutput();
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

    const LONGLONG pts_hns = static_cast<LONGLONG>(in.timestamp_ns / 100ull);
    sample->SetSampleTime(pts_hns);
    sample->SetSampleDuration(10'000'000ll / std::max<UINT32>(1, cfg_.fps));

    if (force_keyframe_.exchange(false, std::memory_order_acq_rel)) {
        sample->SetUINT32(MFSampleExtension_CleanPoint, TRUE);
    }

    // LTR markers — sample-level attributes; symbol availability varies
    // by SDK so we set them via the well-known GUIDs when present.
    if (in.opts.mark_ltr >= 0) {
        sample->SetUINT32(MFSampleExtension_LongTermReferenceFrameInfo,
                          static_cast<UINT32>(in.opts.mark_ltr));
    }
    if (in.opts.use_ltr >= 0 && codec_api_) {
        SetUInt32(codec_api_.Get(), CODECAPI_AVEncVideoUseLTRFrame,
                  static_cast<ULONG>(in.opts.use_ltr));
    }

    // ROI rectangles → sample blob attribute. The encoder reads it on
    // ProcessInput when CODECAPI_AVEncVideoROIEnabled is on.
    if (in.opts.roi_count > 0 && in.opts.roi_rects) {
        sample->SetBlob(MFSampleExtension_ROIRectangle,
                        reinterpret_cast<const UINT8*>(in.opts.roi_rects),
                        static_cast<UINT32>(in.opts.roi_count * 4 * sizeof(std::int32_t)));
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

void MfEncoder::DrainOutputUnsafe() { /* unused */ }

} // namespace wincap
