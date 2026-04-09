#include "core/gfx/d3d_device.h"

#include "core/common/errors.h"

#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>
#include <windows.graphics.directx.direct3d11.interop.h>

#include <d3d11.h>

namespace wincap {

namespace {

Microsoft::WRL::ComPtr<IDXGIAdapter4> PickAdapter(LUID preferred) {
    Microsoft::WRL::ComPtr<IDXGIFactory6> factory;
    WINCAP_THROW_IF_FAILED("d3d_device",
        CreateDXGIFactory2(0, IID_PPV_ARGS(factory.GetAddressOf())));

    if (preferred.LowPart != 0 || preferred.HighPart != 0) {
        Microsoft::WRL::ComPtr<IDXGIAdapter1> a1;
        for (UINT i = 0; factory->EnumAdapters1(i, a1.ReleaseAndGetAddressOf()) != DXGI_ERROR_NOT_FOUND; ++i) {
            DXGI_ADAPTER_DESC1 desc{};
            if (FAILED(a1->GetDesc1(&desc))) continue;
            if (desc.AdapterLuid.LowPart == preferred.LowPart &&
                desc.AdapterLuid.HighPart == preferred.HighPart) {
                Microsoft::WRL::ComPtr<IDXGIAdapter4> a4;
                if (SUCCEEDED(a1.As(&a4))) return a4;
            }
        }
    }

    Microsoft::WRL::ComPtr<IDXGIAdapter1> a1;
    WINCAP_THROW_IF_FAILED("d3d_device",
        factory->EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
                                            IID_PPV_ARGS(a1.GetAddressOf())));
    Microsoft::WRL::ComPtr<IDXGIAdapter4> a4;
    WINCAP_THROW_IF_FAILED("d3d_device", a1.As(&a4));
    return a4;
}

} // namespace

void D3DDevice::Create(LUID preferred_luid) {
    adapter_ = PickAdapter(preferred_luid);

    UINT flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT |
                 D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
#ifndef NDEBUG
    // Debug layer is opt-in; only enable when the SDK layer is installed.
    // flags |= D3D11_CREATE_DEVICE_DEBUG;
#endif

    const D3D_FEATURE_LEVEL levels[] = {
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
    };

    Microsoft::WRL::ComPtr<ID3D11Device>        base_device;
    Microsoft::WRL::ComPtr<ID3D11DeviceContext> base_context;
    D3D_FEATURE_LEVEL got_level{};

    WINCAP_THROW_IF_FAILED("d3d_device",
        D3D11CreateDevice(adapter_.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                          flags, levels, _countof(levels), D3D11_SDK_VERSION,
                          base_device.GetAddressOf(), &got_level,
                          base_context.GetAddressOf()));

    WINCAP_THROW_IF_FAILED("d3d_device", base_device.As(&device_));
    WINCAP_THROW_IF_FAILED("d3d_device", base_context.As(&context_));

    // Multithread-protect the immediate context — WGC FrameArrived runs on
    // an MTA worker and may touch the same context as the consumer thread.
    Microsoft::WRL::ComPtr<ID3D10Multithread> mt;
    if (SUCCEEDED(context_.As(&mt))) {
        mt->SetMultithreadProtected(TRUE);
    }

    // Create the WinRT projection (IDirect3DDevice) used by WGC.
    Microsoft::WRL::ComPtr<IDXGIDevice> dxgi_device;
    WINCAP_THROW_IF_FAILED("d3d_device", device_.As(&dxgi_device));

    using winrt::Windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    Microsoft::WRL::ComPtr<::IInspectable> inspectable;
    WINCAP_THROW_IF_FAILED("d3d_device",
        CreateDirect3D11DeviceFromDXGIDevice(dxgi_device.Get(), inspectable.GetAddressOf()));

    // Hand off ownership to a raw void*; consumer reinterprets to
    // winrt::Windows::Graphics::DirectX::Direct3D11::IDirect3DDevice.
    winrt_device_ = inspectable.Detach();
}

void D3DDevice::Reset() noexcept {
    if (winrt_device_) {
        static_cast<::IUnknown*>(winrt_device_)->Release();
        winrt_device_ = nullptr;
    }
    context_.Reset();
    device_.Reset();
    adapter_.Reset();
}

} // namespace wincap
