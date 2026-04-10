// WAVEFORMATEX / WAVEFORMATEXTENSIBLE helpers. WASAPI loopback returns
// the device mix format (typically 48 kHz float32 stereo). We never
// resample in this layer — that is the encoder/transport's job.
#pragma once

#include <cstdint>

// mmreg.h / audioclient.h depend on NEAR/FAR/WORD/etc from windef.h.
// windows.h must come first or every struct member tokenises as garbage.
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

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
