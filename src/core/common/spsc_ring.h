// Lock-free single-producer / single-consumer bounded ring buffer.
// Capacity is a compile-time power of two. Cache-line padded to avoid
// false sharing between producer and consumer cursors.
//
// Memory ordering follows the canonical Vyukov SPSC pattern:
//   producer: load tail (acquire), store head (release)
//   consumer: load head (acquire), store tail (release)
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <new>
#include <optional>
#include <type_traits>
#include <utility>

namespace wincap {

#if defined(__cpp_lib_hardware_interference_size)
inline constexpr std::size_t kCacheLine = std::hardware_destructive_interference_size;
#else
inline constexpr std::size_t kCacheLine = 64;
#endif

template <typename T, std::size_t Capacity>
class SpscRing {
    static_assert((Capacity & (Capacity - 1)) == 0, "Capacity must be a power of two");
    static_assert(Capacity >= 2, "Capacity must be >= 2");

public:
    SpscRing() = default;
    SpscRing(const SpscRing&) = delete;
    SpscRing& operator=(const SpscRing&) = delete;

    // Producer: try to push. Returns false if full.
    bool TryPush(T value) noexcept(std::is_nothrow_move_constructible_v<T>) {
        const auto head = head_.load(std::memory_order_relaxed);
        const auto next = (head + 1) & kMask;
        if (next == tail_.load(std::memory_order_acquire)) {
            return false; // full
        }
        slots_[head].value.~T();
        ::new (&slots_[head].value) T(std::move(value));
        head_.store(next, std::memory_order_release);
        return true;
    }

    // Producer: push, evicting the oldest if full. Returns true if an
    // existing element was overwritten (used for video drop-oldest policy).
    bool PushOverwrite(T value) noexcept(std::is_nothrow_move_constructible_v<T>) {
        if (TryPush(std::move(value))) return false;
        // Full: pop one then push.
        T discard;
        (void)TryPop(discard);
        TryPush(std::move(value));
        return true;
    }

    // Consumer: try to pop into out. Returns false if empty.
    bool TryPop(T& out) noexcept(std::is_nothrow_move_assignable_v<T>) {
        const auto tail = tail_.load(std::memory_order_relaxed);
        if (tail == head_.load(std::memory_order_acquire)) {
            return false; // empty
        }
        out = std::move(slots_[tail].value);
        const auto next = (tail + 1) & kMask;
        tail_.store(next, std::memory_order_release);
        return true;
    }

    bool Empty() const noexcept {
        return head_.load(std::memory_order_acquire) == tail_.load(std::memory_order_acquire);
    }

    static constexpr std::size_t Capacity_v = Capacity;

private:
    static constexpr std::size_t kMask = Capacity - 1;

    struct alignas(kCacheLine) Slot {
        T value{};
    };

    Slot slots_[Capacity];

    alignas(kCacheLine) std::atomic<std::size_t> head_{0}; // producer
    alignas(kCacheLine) std::atomic<std::size_t> tail_{0}; // consumer
    char pad_[kCacheLine]{};
};

} // namespace wincap
