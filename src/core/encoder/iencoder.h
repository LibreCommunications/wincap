// Encoder interface. The capture path hands NV12 textures in; encoded
// access units come out via a callback. The implementation owns its
// own MF work queue, so callbacks fire on a non-JS thread.
#pragma once

#include <cstdint>
#include <functional>

#include <d3d11_4.h>

namespace wincap {

enum class VideoCodec {
    H264,
    HEVC,
    AV1,
};

struct EncoderConfig {
    VideoCodec    codec{VideoCodec::H264};
    std::uint32_t width{0};
    std::uint32_t height{0};
    std::uint32_t fps{60};
    std::uint32_t bitrate_bps{6'000'000};
    std::uint32_t keyframe_interval_ms{2000};
};

struct EncodedAccessUnit {
    const std::uint8_t* data{nullptr};
    std::size_t         size{0};
    std::uint64_t       timestamp_ns{0};
    bool                keyframe{false};
};

using EncodedCallback = std::function<void(const EncodedAccessUnit&)>;
using EncoderErrorCallback =
    std::function<void(const char* component, long hresult, const char* message)>;

class IEncoder {
public:
    virtual ~IEncoder() = default;

    virtual void Initialize(ID3D11Device5* device, const EncoderConfig& cfg) = 0;
    virtual void Start(EncodedCallback out, EncoderErrorCallback err) = 0;
    virtual void Stop() = 0;

    // Submit one input texture (NV12). `timestamp_ns` is the QPC ns
    // timestamp from the capture path; it becomes the encoded sample's
    // PTS so the consumer can A/V-sync against audio.
    virtual void EncodeFrame(ID3D11Texture2D* nv12, std::uint64_t timestamp_ns) = 0;

    // Hot-swap helpers — both safe to call from any thread.
    virtual void RequestKeyframe() = 0;
    virtual void SetBitrate(std::uint32_t bps) = 0;
};

} // namespace wincap
