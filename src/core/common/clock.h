// Monotonic QPC-based clock shared across capture, audio, and encoder paths.
// All wincap timestamps are nanoseconds since an arbitrary fixed epoch
// (process start). Both WGC SystemRelativeTime and WASAPI qpcPosition are
// already in QPC units, so no rebasing is needed — they share this epoch.
#pragma once

#include <cstdint>

namespace wincap {

class Clock {
public:
    // Initialise the global clock. Idempotent, thread-safe.
    static void Init() noexcept;

    // QPC frequency in ticks/second.
    static std::uint64_t Frequency() noexcept;

    // Current QPC counter.
    static std::uint64_t NowTicks() noexcept;

    // Current time in nanoseconds since the QPC epoch.
    static std::uint64_t NowNs() noexcept;

    // Convert a raw QPC tick value (e.g. from WASAPI qpcPosition or
    // WinRT TimeSpan-as-100ns scaled appropriately) to nanoseconds.
    static std::uint64_t TicksToNs(std::uint64_t ticks) noexcept;

    // Convert a WinRT TimeSpan (100-ns units) to nanoseconds. WGC's
    // Direct3D11CaptureFrame::SystemRelativeTime() returns this.
    static constexpr std::uint64_t HundredNsToNs(std::int64_t hns) noexcept {
        return static_cast<std::uint64_t>(hns) * 100ull;
    }
};

} // namespace wincap
