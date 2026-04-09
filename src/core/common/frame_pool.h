// Pool of GPU textures used by the capture path. Each slot owns:
//   - a D3D11 texture sized to the capture item
//   - an ID3D11Query event fence so the consumer can wait for GPU completion
//   - an optional NT shared handle for cross-API zero-copy delivery
//   - a refcount: capture thread acquires, JS finalizer releases
//
// The pool is created on the capture thread and accessed from at most one
// producer (capture) + one consumer (marshaller) at a time. Slot acquisition
// is lock-free; the free list is a single atomic bitmask (capacity <= 32).
#pragma once

#include <array>
#include <atomic>
#include <cstdint>
#include <wrl/client.h>

#include <d3d11_4.h>
#include <dxgi1_6.h>

namespace wincap {

struct FrameSlot {
    Microsoft::WRL::ComPtr<ID3D11Texture2D> texture;
    Microsoft::WRL::ComPtr<ID3D11Query>     fence;
    HANDLE                                  shared_nt{nullptr};
    std::uint64_t                           keyed_mutex_key{0};
    std::atomic<std::uint32_t>              refcount{0};
    std::uint32_t                           index{0};
};

class FramePool {
public:
    static constexpr std::uint32_t kMaxSlots = 8;

    FramePool() = default;
    ~FramePool();

    FramePool(const FramePool&) = delete;
    FramePool& operator=(const FramePool&) = delete;

    // Allocate `count` slots of the given description. Must be called once
    // before the capture session starts. Returns false on failure.
    bool Init(ID3D11Device* device,
              std::uint32_t count,
              const D3D11_TEXTURE2D_DESC& desc,
              bool create_shared_handle);

    void Shutdown();

    // Producer: acquire a free slot, marking refcount=1. Returns nullptr
    // if pool is exhausted (caller should drop oldest in its own ring).
    FrameSlot* Acquire() noexcept;

    // Increment refcount; consumer hands a borrowed pointer to JS.
    static void Retain(FrameSlot* slot) noexcept;

    // Decrement; when refcount hits 0 the slot returns to the free list.
    void Release(FrameSlot* slot) noexcept;

    std::uint32_t Capacity() const noexcept { return count_; }

private:
    std::array<FrameSlot, kMaxSlots> slots_{};
    std::uint32_t                    count_{0};
    std::atomic<std::uint32_t>       free_mask_{0}; // bit i set => slot i is free
};

} // namespace wincap
