// Windows.Graphics.Capture (WGC) capture source. Owns a free-threaded
// Direct3D11CaptureFramePool and dispatches FrameArrived events on the
// MTA worker that WinRT provides — no dispatcher queue required.
//
// On each frame: borrow the WGC surface as ID3D11Texture2D, acquire a
// FrameSlot from the pool, GPU-copy via CopySubresourceRegion, schedule a
// fence query, then hand the slot to the consumer callback.
#pragma once

#include "core/capture/icapture_source.h"
#include "core/common/frame_pool.h"
#include "core/gfx/d3d_device.h"

#include <atomic>
#include <cstdint>
#include <memory>

#include <wrl/client.h>
#include <winrt/Windows.Graphics.Capture.h>
#include <winrt/Windows.Graphics.DirectX.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>

namespace wincap {

struct WgcOptions {
    bool include_cursor    = true;
    bool border_required   = false;  // Win11 22H2+
    bool create_shared_handle = false;
    winrt::Windows::Graphics::DirectX::DirectXPixelFormat pixel_format =
        winrt::Windows::Graphics::DirectX::DirectXPixelFormat::B8G8R8A8UIntNormalized;
};

class WgcSource final : public ICaptureSource {
public:
    WgcSource(D3DDevice& device, FramePool& pool, WgcOptions opts);
    ~WgcSource() override;

    // Initialise from a monitor handle (display capture).
    void InitForMonitor(HMONITOR monitor);

    // Initialise from an HWND (window capture).
    void InitForWindow(HWND hwnd);

    void Start(FrameCallback frame_cb, ErrorCallback err_cb) override;
    void Stop() override;

    std::uint32_t Width()  const noexcept override { return width_; }
    std::uint32_t Height() const noexcept override { return height_; }

private:
    void OnFrameArrived(
        winrt::Windows::Graphics::Capture::Direct3D11CaptureFramePool const& sender,
        winrt::Windows::Foundation::IInspectable const& args);

    void OnClosed(
        winrt::Windows::Graphics::Capture::GraphicsCaptureItem const& sender,
        winrt::Windows::Foundation::IInspectable const& args);

    void RecreateFramePool(std::uint32_t width, std::uint32_t height);

    D3DDevice&  device_;
    FramePool&  pool_;
    WgcOptions  opts_;

    winrt::Windows::Graphics::Capture::GraphicsCaptureItem        item_{nullptr};
    winrt::Windows::Graphics::Capture::Direct3D11CaptureFramePool frame_pool_{nullptr};
    winrt::Windows::Graphics::Capture::GraphicsCaptureSession     session_{nullptr};

    winrt::event_token frame_token_{};
    winrt::event_token closed_token_{};

    FrameCallback frame_cb_;
    ErrorCallback err_cb_;

    std::atomic<std::uint32_t> width_{0};
    std::atomic<std::uint32_t> height_{0};
    std::atomic<bool>          running_{false};
};

} // namespace wincap
