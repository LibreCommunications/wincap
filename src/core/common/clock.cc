#include "core/common/clock.h"

#include <atomic>
#include <windows.h>

namespace wincap {

namespace {
std::atomic<std::uint64_t> g_freq{0};

std::uint64_t LoadFreq() noexcept {
    std::uint64_t f = g_freq.load(std::memory_order_acquire);
    if (f != 0) return f;
    LARGE_INTEGER li{};
    QueryPerformanceFrequency(&li);
    f = static_cast<std::uint64_t>(li.QuadPart);
    g_freq.store(f, std::memory_order_release);
    return f;
}
} // namespace

void Clock::Init() noexcept { (void)LoadFreq(); }

std::uint64_t Clock::Frequency() noexcept { return LoadFreq(); }

std::uint64_t Clock::NowTicks() noexcept {
    LARGE_INTEGER li{};
    QueryPerformanceCounter(&li);
    return static_cast<std::uint64_t>(li.QuadPart);
}

std::uint64_t Clock::TicksToNs(std::uint64_t ticks) noexcept {
    const std::uint64_t freq = LoadFreq();
    // Split to avoid 64-bit overflow on large counters:
    //   ns = (whole_seconds * 1e9) + (remainder * 1e9 / freq)
    const std::uint64_t whole = ticks / freq;
    const std::uint64_t rem   = ticks % freq;
    return whole * 1'000'000'000ull + (rem * 1'000'000'000ull) / freq;
}

std::uint64_t Clock::NowNs() noexcept { return TicksToNs(NowTicks()); }

} // namespace wincap
