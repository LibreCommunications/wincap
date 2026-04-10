#include "core/gfx/video_processor.h"

#include "core/common/errors.h"

namespace wincap {

void VideoProcessor::Init(ID3D11Device5*        device,
                          ID3D11DeviceContext4* context,
                          UINT                  width,
                          UINT                  height,
                          DXGI_FORMAT           output_format,
                          ColorSpace            cs) {
    width_         = width;
    height_        = height;
    output_format_ = output_format;

    WINCAP_THROW_IF_FAILED("video_processor", device->QueryInterface(IID_PPV_ARGS(video_device_.GetAddressOf())));
    WINCAP_THROW_IF_FAILED("video_processor", context->QueryInterface(IID_PPV_ARGS(video_context_.GetAddressOf())));

    D3D11_VIDEO_PROCESSOR_CONTENT_DESC desc{};
    desc.InputFrameFormat            = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
    desc.InputFrameRate.Numerator    = 60;
    desc.InputFrameRate.Denominator  = 1;
    desc.InputWidth                  = width;
    desc.InputHeight                 = height;
    desc.OutputFrameRate.Numerator   = 60;
    desc.OutputFrameRate.Denominator = 1;
    desc.OutputWidth                 = width;
    desc.OutputHeight                = height;
    desc.Usage                       = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;

    WINCAP_THROW_IF_FAILED("video_processor",
        video_device_->CreateVideoProcessorEnumerator(&desc, enumerator_.GetAddressOf()));
    WINCAP_THROW_IF_FAILED("video_processor",
        video_device_->CreateVideoProcessor(enumerator_.Get(), 0, processor_.GetAddressOf()));

    if (cs == ColorSpace::Rec709Sdr) {
        D3D11_VIDEO_PROCESSOR_COLOR_SPACE in_cs{};
        in_cs.RGB_Range     = 0;                         // full range
        in_cs.YCbCr_Matrix  = 1;                         // BT.709
        in_cs.Nominal_Range = D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255;
        video_context_->VideoProcessorSetStreamColorSpace(processor_.Get(), 0, &in_cs);

        D3D11_VIDEO_PROCESSOR_COLOR_SPACE out_cs{};
        out_cs.RGB_Range     = 0;
        out_cs.YCbCr_Matrix  = 1;
        out_cs.Nominal_Range = D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235;
        video_context_->VideoProcessorSetOutputColorSpace(processor_.Get(), &out_cs);
    } else {
        // HDR10: scRGB linear float input → BT.2020 PQ studio-range YUV.
        // We use the richer DXGI_COLOR_SPACE path via VideoContext1.
        Microsoft::WRL::ComPtr<ID3D11VideoContext2> ctx2;
        if (SUCCEEDED(video_context_.As(&ctx2))) {
            ctx2->VideoProcessorSetStreamColorSpace1(
                processor_.Get(), 0,
                DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 /* scRGB */);
            ctx2->VideoProcessorSetOutputColorSpace1(
                processor_.Get(),
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020 /* BT.2020 PQ */);
        } else {
            // Fallback (rare on Win10 1809+).
            D3D11_VIDEO_PROCESSOR_COLOR_SPACE in_cs{};
            in_cs.RGB_Range = 0; in_cs.YCbCr_Matrix = 0;
            in_cs.Nominal_Range = D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255;
            video_context_->VideoProcessorSetStreamColorSpace(processor_.Get(), 0, &in_cs);
            D3D11_VIDEO_PROCESSOR_COLOR_SPACE out_cs{};
            out_cs.RGB_Range = 0; out_cs.YCbCr_Matrix = 0;
            out_cs.Nominal_Range = D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235;
            video_context_->VideoProcessorSetOutputColorSpace(processor_.Get(), &out_cs);
        }
    }

    RECT full{0, 0, static_cast<LONG>(width), static_cast<LONG>(height)};
    video_context_->VideoProcessorSetStreamSourceRect(processor_.Get(), 0, TRUE, &full);
    video_context_->VideoProcessorSetStreamDestRect(processor_.Get(),  0, TRUE, &full);
    video_context_->VideoProcessorSetOutputTargetRect(processor_.Get(), TRUE, &full);
}

void VideoProcessor::Convert(ID3D11Texture2D* src, ID3D11Texture2D* dest) {
    if (!processor_ || !src || !dest) return;

    Microsoft::WRL::ComPtr<ID3D11VideoProcessorInputView> in_view;
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC in_desc{};
    in_desc.FourCC         = 0;
    in_desc.ViewDimension  = D3D11_VPIV_DIMENSION_TEXTURE2D;
    in_desc.Texture2D.MipSlice  = 0;
    in_desc.Texture2D.ArraySlice = 0;
    WINCAP_THROW_IF_FAILED("video_processor",
        video_device_->CreateVideoProcessorInputView(src, enumerator_.Get(), &in_desc,
            in_view.GetAddressOf()));

    Microsoft::WRL::ComPtr<ID3D11VideoProcessorOutputView> out_view;
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC out_desc{};
    out_desc.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
    out_desc.Texture2D.MipSlice = 0;
    WINCAP_THROW_IF_FAILED("video_processor",
        video_device_->CreateVideoProcessorOutputView(dest, enumerator_.Get(), &out_desc,
            out_view.GetAddressOf()));

    D3D11_VIDEO_PROCESSOR_STREAM stream{};
    stream.Enable          = TRUE;
    stream.OutputIndex     = 0;
    stream.InputFrameOrField = 0;
    stream.PastFrames      = 0;
    stream.FutureFrames    = 0;
    stream.pInputSurface   = in_view.Get();

    WINCAP_THROW_IF_FAILED("video_processor",
        video_context_->VideoProcessorBlt(processor_.Get(), out_view.Get(), 0, 1, &stream));
}

void VideoProcessor::Reset() noexcept {
    processor_.Reset();
    enumerator_.Reset();
    video_context_.Reset();
    video_device_.Reset();
    width_ = height_ = 0;
}

} // namespace wincap
