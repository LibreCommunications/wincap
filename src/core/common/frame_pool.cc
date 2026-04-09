#include "core/common/frame_pool.h"

#include "core/common/errors.h"

#include <intrin.h>

namespace wincap {

FramePool::~FramePool() { Shutdown(); }

bool FramePool::Init(ID3D11Device* device,
                     std::uint32_t count,
                     const D3D11_TEXTURE2D_DESC& base_desc,
                     bool create_shared_handle) {
    if (!device || count == 0 || count > kMaxSlots) return false;
    count_ = count;

    D3D11_TEXTURE2D_DESC desc = base_desc;
    if (create_shared_handle) {
        desc.MiscFlags |= D3D11_RESOURCE_MISC_SHARED_NTHANDLE |
                          D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX;
    }

    for (std::uint32_t i = 0; i < count; ++i) {
        FrameSlot& slot = slots_[i];
        slot.index = i;
        slot.refcount.store(0, std::memory_order_relaxed);

        WINCAP_THROW_IF_FAILED("frame_pool",
            device->CreateTexture2D(&desc, nullptr, slot.texture.ReleaseAndGetAddressOf()));

        D3D11_QUERY_DESC qd{};
        qd.Query = D3D11_QUERY_EVENT;
        WINCAP_THROW_IF_FAILED("frame_pool",
            device->CreateQuery(&qd, slot.fence.ReleaseAndGetAddressOf()));

        if (create_shared_handle) {
            Microsoft::WRL::ComPtr<IDXGIResource1> dxgi_res;
            WINCAP_THROW_IF_FAILED("frame_pool",
                slot.texture.As(&dxgi_res));
            WINCAP_THROW_IF_FAILED("frame_pool",
                dxgi_res->CreateSharedHandle(
                    nullptr,
                    DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE,
                    nullptr,
                    &slot.shared_nt));
        }
    }

    // Mark all slots free.
    free_mask_.store((1u << count) - 1u, std::memory_order_release);
    return true;
}

void FramePool::Shutdown() {
    for (std::uint32_t i = 0; i < count_; ++i) {
        if (slots_[i].shared_nt) {
            ::CloseHandle(slots_[i].shared_nt);
            slots_[i].shared_nt = nullptr;
        }
        slots_[i].texture.Reset();
        slots_[i].fence.Reset();
    }
    count_ = 0;
    free_mask_.store(0, std::memory_order_release);
}

FrameSlot* FramePool::Acquire() noexcept {
    std::uint32_t mask = free_mask_.load(std::memory_order_acquire);
    while (mask != 0) {
        unsigned long bit;
        _BitScanForward(&bit, mask);
        const std::uint32_t want = mask & ~(1u << bit);
        if (free_mask_.compare_exchange_weak(mask, want,
                std::memory_order_acq_rel, std::memory_order_acquire)) {
            FrameSlot* slot = &slots_[bit];
            slot->refcount.store(1, std::memory_order_release);
            return slot;
        }
    }
    return nullptr;
}

void FramePool::Retain(FrameSlot* slot) noexcept {
    if (!slot) return;
    slot->refcount.fetch_add(1, std::memory_order_acq_rel);
}

void FramePool::Release(FrameSlot* slot) noexcept {
    if (!slot) return;
    if (slot->refcount.fetch_sub(1, std::memory_order_acq_rel) == 1) {
        free_mask_.fetch_or(1u << slot->index, std::memory_order_release);
    }
}

} // namespace wincap
