// Abstract capture source — implemented by wgc_source (primary) and
// ddup_source (fallback). Frames are delivered to the owner via a callback
// invoked on the capture thread; the callback must NOT block.
#pragma once

#include "core/common/frame_pool.h"

#include <cstdint>
#include <functional>

namespace wincap {

struct CapturedFrame {
    FrameSlot*    slot{nullptr};      // borrowed; consumer must Release()
    std::uint32_t width{0};
    std::uint32_t height{0};
    std::uint64_t timestamp_ns{0};    // QPC epoch
    bool          size_changed{false};
};

using FrameCallback = std::function<void(const CapturedFrame&)>;
using ErrorCallback = std::function<void(const char* component, long hresult, const char* message)>;

class ICaptureSource {
public:
    virtual ~ICaptureSource() = default;

    virtual void Start(FrameCallback frame_cb, ErrorCallback err_cb) = 0;
    virtual void Stop() = 0;

    virtual std::uint32_t Width()  const noexcept = 0;
    virtual std::uint32_t Height() const noexcept = 0;
};

} // namespace wincap
