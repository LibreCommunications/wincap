// Abstract audio capture source. Implementations: wasapi_loopback (system
// device loopback) and the same class with PROCESS_LOOPBACK activation
// params (Win11 22000+) for per-process capture.
//
// Audio chunks are delivered on the audio thread; the callback must be
// non-blocking. Buffers are owned by an internal pool — the consumer
// must call AudioChunk::release_fn(opaque) when done.
#pragma once

#include <cstdint>
#include <functional>

namespace wincap {

struct AudioChunk {
    const float*  data{nullptr};   // interleaved float32, channel-major within frames
    std::uint32_t frame_count{0};
    std::uint32_t channels{0};
    std::uint32_t sample_rate{0};
    std::uint64_t timestamp_ns{0}; // QPC epoch
    bool          silent{false};
    bool          discontinuity{false};

    // Lifetime: when the consumer is done, call release_fn(opaque).
    void (*release_fn)(void* opaque) {nullptr};
    void* release_opaque{nullptr};
};

using AudioCallback = std::function<void(const AudioChunk&)>;
using AudioErrorCallback =
    std::function<void(const char* component, long hresult, const char* message)>;

class IAudioSource {
public:
    virtual ~IAudioSource() = default;
    virtual void Start(AudioCallback cb, AudioErrorCallback err) = 0;
    virtual void Stop() = 0;
};

} // namespace wincap
