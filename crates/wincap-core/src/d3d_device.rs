use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;

use crate::error::{hr_call, WincapError, WincapResult};

pub struct D3DDevice {
    pub device: ID3D11Device5,
    pub context: ID3D11DeviceContext4,
    pub adapter: IDXGIAdapter4,
    winrt_device: IDirect3DDevice,
}

// SAFETY: D3D11 device with multithread protection enabled is thread-safe.
unsafe impl Send for D3DDevice {}
unsafe impl Sync for D3DDevice {}

impl D3DDevice {
    /// Create a D3D11 device. If `preferred_luid` is non-zero, picks the
    /// matching adapter; otherwise uses the high-performance adapter.
    pub fn create(preferred_luid: LUID) -> WincapResult<Self> {
        let adapter = pick_adapter(preferred_luid)?;

        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];

        let mut base_device = None;
        let mut base_context = None;
        let mut _got_level = D3D_FEATURE_LEVEL_11_0;

        hr_call!("d3d_device", unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                flags,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut base_device),
                Some(&mut _got_level),
                Some(&mut base_context),
            )
        });

        let base_device = base_device.ok_or_else(|| WincapError::General {
            component: "d3d_device",
            message: "D3D11CreateDevice returned no device".into(),
        })?;
        let base_context = base_context.ok_or_else(|| WincapError::General {
            component: "d3d_device",
            message: "D3D11CreateDevice returned no context".into(),
        })?;

        let device: ID3D11Device5 = hr_call!("d3d_device", base_device.cast());
        let context: ID3D11DeviceContext4 = hr_call!("d3d_device", base_context.cast());

        // Multithread-protect the immediate context.
        if let Ok(mt) = context.cast::<windows::Win32::Graphics::Direct3D10::ID3D10Multithread>() {
            unsafe { mt.SetMultithreadProtected(true) };
        }

        // Create the WinRT projection used by WGC.
        let dxgi_device: IDXGIDevice = hr_call!("d3d_device", device.cast());
        let inspectable =
            hr_call!("d3d_device", unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) });
        let winrt_device: IDirect3DDevice = hr_call!("d3d_device", inspectable.cast());

        Ok(Self {
            device,
            context,
            adapter,
            winrt_device,
        })
    }

    pub fn winrt_device(&self) -> &IDirect3DDevice {
        &self.winrt_device
    }

    /// Get the underlying D3D11 texture from a WinRT IDirect3DSurface.
    pub fn surface_to_texture(
        surface: &windows::Graphics::DirectX::Direct3D11::IDirect3DSurface,
    ) -> WincapResult<ID3D11Texture2D> {
        let access: IDirect3DDxgiInterfaceAccess = hr_call!("d3d_device", surface.cast());
        let texture: ID3D11Texture2D = hr_call!("d3d_device", unsafe { access.GetInterface() });
        Ok(texture)
    }
}

fn pick_adapter(preferred: LUID) -> WincapResult<IDXGIAdapter4> {
    let factory: IDXGIFactory6 =
        hr_call!("d3d_device", unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) });

    // Try to find the preferred LUID adapter.
    if preferred.LowPart != 0 || preferred.HighPart != 0 {
        let mut i = 0u32;
        loop {
            let result: Result<IDXGIAdapter1, _> = unsafe { factory.EnumAdapters1(i) };
            match result {
                Ok(adapter) => {
                    if let Ok(desc) = unsafe { adapter.GetDesc1() } {
                        if desc.AdapterLuid.LowPart == preferred.LowPart
                            && desc.AdapterLuid.HighPart == preferred.HighPart
                        {
                            if let Ok(a4) = adapter.cast::<IDXGIAdapter4>() {
                                return Ok(a4);
                            }
                        }
                    }
                    i += 1;
                }
                Err(_) => break,
            }
        }
    }

    // Fallback: high-performance adapter.
    let adapter: IDXGIAdapter1 = hr_call!("d3d_device", unsafe {
        factory.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)
    });
    let a4: IDXGIAdapter4 = hr_call!("d3d_device", adapter.cast());
    Ok(a4)
}
