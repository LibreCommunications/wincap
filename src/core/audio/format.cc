#include "core/audio/format.h"

#include <ksmedia.h>

namespace wincap {

AudioFormat AudioFormat::FromWaveFormat(const WAVEFORMATEX* wf) noexcept {
    AudioFormat f{};
    if (!wf) return f;
    f.sample_rate     = wf->nSamplesPerSec;
    f.channels        = wf->nChannels;
    f.bits_per_sample = wf->wBitsPerSample;
    f.float32         = false;
    if (wf->wFormatTag == WAVE_FORMAT_IEEE_FLOAT) {
        f.float32 = true;
    } else if (wf->wFormatTag == WAVE_FORMAT_EXTENSIBLE &&
               wf->cbSize >= 22) {
        const auto* ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(wf);
        f.float32 = (ext->SubFormat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
    }
    return f;
}

} // namespace wincap
