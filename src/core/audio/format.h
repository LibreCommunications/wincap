// WAVEFORMATEX / WAVEFORMATEXTENSIBLE helpers. WASAPI loopback returns
// the device mix format (typically 48 kHz float32 stereo). We never
// resample in this layer — that is the encoder/transport's job.
#pragma once

#include <cstdint>

#include <mmreg.h>
#include <audioclient.h>

namespace wincap {

struct AudioFormat {
    std::uint32_t sample_rate{0};
    std::uint32_t channels{0};
    bool          float32{true};
    std::uint32_t bits_per_sample{32};

    static AudioFormat FromWaveFormat(const WAVEFORMATEX* wf) noexcept;
    std::uint32_t BytesPerFrame() const noexcept {
        return channels * (bits_per_sample / 8);
    }
};

} // namespace wincap
