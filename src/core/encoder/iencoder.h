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

    // HDR10 (Main10 / 10-bit). Forces P010 input. Codec must be HEVC or
    // AV1 (H.264 has no 10-bit profile in MS encoders).
    bool          hdr10{false};

    // Long-term reference frames for RTC packet-loss recovery.
    // 0 disables. Typical value: 4–8.
    std::uint32_t ltr_count{0};

    // Intra refresh: when enabled the encoder spreads I-block coverage
    // across `intra_refresh_period` frames instead of emitting full IDRs.
    // Eliminates bitrate spikes; required for smooth low-latency streams.
    bool          intra_refresh{false};
    std::uint32_t intra_refresh_period{60};

    // Per-frame ROI: dirty rectangles get higher quality, the rest is
    // skipped/coarse. The encoder must support CODECAPI_AVEncVideoROIEnabled.
    bool          roi_enabled{false};
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

    struct FrameOptions {
        // Mark this frame as a long-term reference (0..ltr_count-1, or
        // -1 to leave alone). The transport tracks acks so the next
        // UseLtr() can target a known-received reference.
        int           mark_ltr{-1};
        // Reference an existing LTR slot (0..ltr_count-1, or -1).
        int           use_ltr{-1};
        // Optional ROI rectangles in pixel space. When the encoder has
        // ROI enabled the listed regions are coded at higher quality.
        const std::int32_t* roi_rects{nullptr};
        std::size_t         roi_count{0}; // count of rects (4 ints each: l,t,r,b)
    };

    // Submit one input texture. Format must match the encoder's input
    // (NV12 for SDR, P010 for HDR10). `timestamp_ns` is the QPC ns
    // timestamp from the capture path; it becomes the encoded sample's
    // PTS so the consumer can A/V-sync against audio.
    virtual void EncodeFrame(ID3D11Texture2D* surface,
                             std::uint64_t timestamp_ns,
                             const FrameOptions& opts = {}) = 0;

    // Hot-swap helpers — all safe to call from any thread.
    virtual void RequestKeyframe() = 0;
    virtual void SetBitrate(std::uint32_t bps) = 0;
};

} // namespace wincap
