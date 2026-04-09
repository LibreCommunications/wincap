#include "core/capture/wgc_source.h"

#include "core/common/clock.h"
#include "core/common/errors.h"

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.h>
#include <winrt/Windows.Graphics.Capture.h>

#include <windows.graphics.capture.interop.h>
#include <windows.graphics.directx.direct3d11.interop.h>

#include <inspectable.h>

namespace wgc  = winrt::Windows::Graphics::Capture;
namespace wgdx = winrt::Windows::Graphics::DirectX;
namespace wgd  = winrt::Windows::Graphics::DirectX::Direct3D11;
namespace wf   = winrt::Windows::Foundation;
namespace wg   = winrt::Windows::Graphics;

namespace wincap {

namespace {

template <typename T>
auto AsWinRt(void* raw) {
    // Wrap a raw IInspectable* (owned, +1 ref) into a winrt smart ptr.
    winrt::com_ptr<::IInspectable> ins;
    ins.copy_from(static_cast<::IInspectable*>(raw));
    return ins.as<T>();
}

Microsoft::WRL::ComPtr<ID3D11Texture2D> SurfaceToTexture(
    wgd::IDirect3DSurface const& surface) {
    auto access = surface.as<::Windows::Graphics::DirectX::Direct3D11::
        IDirect3DDxgiInterfaceAccess>();
    Microsoft::WRL::ComPtr<ID3D11Texture2D> tex;
    WINCAP_THROW_IF_FAILED("wgc_source",
        access->GetInterface(IID_PPV_ARGS(tex.GetAddressOf())));
    return tex;
}

} // namespace

WgcSource::WgcSource(D3DDevice& device, FramePool& pool, WgcOptions opts)
    : device_(device), pool_(pool), opts_(opts) {}

WgcSource::~WgcSource() { Stop(); }

void WgcSource::InitForMonitor(HMONITOR monitor) {
    auto interop = winrt::get_activation_factory<wgc::GraphicsCaptureItem,
        ::IGraphicsCaptureItemInterop>();
    winrt::com_ptr<::IInspectable> ins;
    WINCAP_THROW_IF_FAILED("wgc_source",
        interop->CreateForMonitor(monitor,
            winrt::guid_of<wgc::GraphicsCaptureItem>(),
            winrt::put_abi(item_)));
}

void WgcSource::InitForWindow(HWND hwnd) {
    auto interop = winrt::get_activation_factory<wgc::GraphicsCaptureItem,
        ::IGraphicsCaptureItemInterop>();
    WINCAP_THROW_IF_FAILED("wgc_source",
        interop->CreateForWindow(hwnd,
            winrt::guid_of<wgc::GraphicsCaptureItem>(),
            winrt::put_abi(item_)));
}

void WgcSource::RecreateFramePool(std::uint32_t width, std::uint32_t height) {
    auto winrt_device = AsWinRt<wgd::IDirect3DDevice>(device_.WinRtDevice());

    if (frame_pool_) {
        frame_pool_.Recreate(winrt_device, opts_.pixel_format, 3,
                             { static_cast<int32_t>(width), static_cast<int32_t>(height) });
    } else {
        frame_pool_ = wgc::Direct3D11CaptureFramePool::CreateFreeThreaded(
            winrt_device, opts_.pixel_format, 3,
            { static_cast<int32_t>(width), static_cast<int32_t>(height) });
    }
    width_.store(width, std::memory_order_release);
    height_.store(height, std::memory_order_release);
}

void WgcSource::Start(FrameCallback frame_cb, ErrorCallback err_cb) {
    if (running_.exchange(true)) return;
    frame_cb_ = std::move(frame_cb);
    err_cb_   = std::move(err_cb);

    if (!item_) {
        throw HrError(E_UNEXPECTED, "wgc_source", "capture item not initialised");
    }

    const auto size = item_.Size();
    RecreateFramePool(static_cast<std::uint32_t>(size.Width),
                      static_cast<std::uint32_t>(size.Height));

    frame_token_ = frame_pool_.FrameArrived({this, &WgcSource::OnFrameArrived});
    closed_token_ = item_.Closed({this, &WgcSource::OnClosed});

    session_ = frame_pool_.CreateCaptureSession(item_);

    // Optional features — feature-gated by Windows build at runtime; the
    // try/catch absorbs ABI-missing properties on older systems.
    try { session_.IsCursorCaptureEnabled(opts_.include_cursor); } catch (...) {}
    try { session_.IsBorderRequired(opts_.border_required); } catch (...) {}

    session_.StartCapture();
}

void WgcSource::Stop() {
    if (!running_.exchange(false)) return;

    if (frame_pool_ && frame_token_) {
        frame_pool_.FrameArrived(frame_token_);
        frame_token_ = {};
    }
    if (item_ && closed_token_) {
        item_.Closed(closed_token_);
        closed_token_ = {};
    }
    if (session_) { session_.Close(); session_ = nullptr; }
    if (frame_pool_) { frame_pool_.Close(); frame_pool_ = nullptr; }
    item_ = nullptr;

    frame_cb_ = nullptr;
    err_cb_   = nullptr;
}

void WgcSource::OnClosed(wgc::GraphicsCaptureItem const&,
                         wf::IInspectable const&) {
    if (err_cb_) err_cb_("wgc_source", 0, "capture item closed");
}

void WgcSource::OnFrameArrived(wgc::Direct3D11CaptureFramePool const& sender,
                               wf::IInspectable const&) {
    if (!running_.load(std::memory_order_acquire)) return;

    try {
        auto frame = sender.TryGetNextFrame();
        if (!frame) return;

        const auto content_size = frame.ContentSize();
        const auto w = static_cast<std::uint32_t>(content_size.Width);
        const auto h = static_cast<std::uint32_t>(content_size.Height);

        bool size_changed = false;
        if (w != width_.load(std::memory_order_acquire) ||
            h != height_.load(std::memory_order_acquire)) {
            // Hand-off: caller's pool needs to resize too. We bubble the
            // size_changed flag and skip this frame; the consumer will
            // recreate the pool and we will Recreate ours below.
            size_changed = true;
            RecreateFramePool(w, h);
        }

        FrameSlot* slot = pool_.Acquire();
        if (!slot) {
            // Pool exhausted: drop this frame.
            return;
        }

        auto src_tex = SurfaceToTexture(frame.Surface());
        auto* ctx = device_.Context();

        D3D11_BOX box{};
        box.left = 0; box.top = 0; box.front = 0;
        box.right = w; box.bottom = h; box.back = 1;
        ctx->CopySubresourceRegion(slot->texture.Get(), 0, 0, 0, 0,
                                   src_tex.Get(), 0, &box);
        ctx->End(slot->fence.Get());

        CapturedFrame out{};
        out.slot         = slot;
        out.width        = w;
        out.height       = h;
        out.timestamp_ns = Clock::HundredNsToNs(frame.SystemRelativeTime().count());
        out.size_changed = size_changed;

        if (frame_cb_) frame_cb_(out);
        else pool_.Release(slot);
    } catch (HrError const& e) {
        if (err_cb_) err_cb_(e.component(), e.hr(), e.what());
    } catch (winrt::hresult_error const& e) {
        if (err_cb_) err_cb_("wgc_source", e.code().value, "winrt error");
    } catch (...) {
        if (err_cb_) err_cb_("wgc_source", E_FAIL, "unknown error");
    }
}

} // namespace wincap
