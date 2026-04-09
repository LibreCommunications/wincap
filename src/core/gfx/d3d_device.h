// D3D11 device + immediate context, plus the WinRT IDirect3DDevice
// projection that WGC's Direct3D11CaptureFramePool::Create requires.
//
// Adapter selection: prefer the adapter associated with the LUID of the
// target output (avoids cross-GPU copies on hybrid laptops). Fall back to
// the default adapter when the LUID is unknown (e.g. window capture).
#pragma once

#include <wrl/client.h>

#include <d3d11_4.h>
#include <dxgi1_6.h>

namespace winrt::Windows::Graphics::DirectX::Direct3D11 {
struct IDirect3DDevice;
}

namespace wincap {

class D3DDevice {
public:
    D3DDevice() = default;
    ~D3DDevice() = default;

    D3DDevice(const D3DDevice&) = delete;
    D3DDevice& operator=(const D3DDevice&) = delete;

    // Create a device. If `preferred_luid` is non-zero, picks the matching
    // adapter; otherwise uses adapter index 0.
    void Create(LUID preferred_luid);

    ID3D11Device5*        Device()  const noexcept { return device_.Get(); }
    ID3D11DeviceContext4* Context() const noexcept { return context_.Get(); }
    IDXGIAdapter4*        Adapter() const noexcept { return adapter_.Get(); }

    // WinRT projection of the device, used to create
    // Direct3D11CaptureFramePool. Returned as void* to keep this header free
    // of cppwinrt; the .cc casts to the real type.
    void* WinRtDevice() const noexcept { return winrt_device_; }

    void Reset() noexcept;

private:
    Microsoft::WRL::ComPtr<ID3D11Device5>        device_;
    Microsoft::WRL::ComPtr<ID3D11DeviceContext4> context_;
    Microsoft::WRL::ComPtr<IDXGIAdapter4>        adapter_;
    void*                                        winrt_device_{nullptr}; // owns +1 ref
};

} // namespace wincap
