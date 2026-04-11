use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_RATIONAL};

use crate::error::{hr_call, WincapResult};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// BGRA8 -> NV12, BT.709 limited range (SDR)
    Rec709Sdr,
    /// R16G16B16A16Float -> P010, BT.2020 PQ (HDR10)
    Rec2020Pq,
}

pub struct VideoProcessor {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    width: u32,
    height: u32,
}

// SAFETY: D3D11 video processor with multithread-protected context.
unsafe impl Send for VideoProcessor {}
unsafe impl Sync for VideoProcessor {}

impl VideoProcessor {
    pub fn new(
        device: &ID3D11Device5,
        context: &ID3D11DeviceContext4,
        width: u32,
        height: u32,
        _output_format: DXGI_FORMAT,
        cs: ColorSpace,
    ) -> WincapResult<Self> {
        let video_device: ID3D11VideoDevice = hr_call!("video_processor", device.cast());
        let video_context: ID3D11VideoContext1 = hr_call!("video_processor", context.cast());

        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            InputWidth: width,
            InputHeight: height,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };

        let enumerator: ID3D11VideoProcessorEnumerator =
            hr_call!("video_processor", unsafe { video_device.CreateVideoProcessorEnumerator(&desc) });
        let processor: ID3D11VideoProcessor =
            hr_call!("video_processor", unsafe { video_device.CreateVideoProcessor(&enumerator, 0) });

        if cs == ColorSpace::Rec709Sdr {
            let _in_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                _bitfield: 0, // RGB_Range=0 (full), YCbCr_Matrix=1 (BT.709)
            };
            // Set bits: Usage=0, RGB_Range=0, YCbCr_Matrix=1, YCbCr_xvYCC=0, Nominal_Range=0_255
            let mut in_cs_val = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
            in_cs_val._bitfield = 0b0_0_01_0_01; // YCbCr_Matrix=1, Nominal_Range=0_255
            unsafe {
                video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &in_cs_val);
            }

            let mut out_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
            out_cs._bitfield = 0b0_0_01_0_10; // YCbCr_Matrix=1, Nominal_Range=16_235
            unsafe {
                video_context.VideoProcessorSetOutputColorSpace(&processor, &out_cs);
            }
        } else {
            // HDR10: scRGB linear float input -> BT.2020 PQ studio-range YUV.
            // Use the DXGI_COLOR_SPACE path via VideoContext2 if available.
            if let Ok(ctx2) = video_context.cast::<ID3D11VideoContext2>() {
                use windows::Win32::Graphics::Dxgi::Common::*;
                unsafe {
                    ctx2.VideoProcessorSetStreamColorSpace1(
                        &processor,
                        0,
                        DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709, // scRGB
                    );
                    ctx2.VideoProcessorSetOutputColorSpace1(
                        &processor,
                        DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020, // BT.2020 PQ
                    );
                }
            } else {
                // Fallback for older systems.
                let mut in_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
                in_cs._bitfield = 0b0_0_00_0_01; // Full range
                unsafe {
                    video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &in_cs);
                }
                let mut out_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
                out_cs._bitfield = 0b0_0_00_0_10; // 16-235
                unsafe {
                    video_context.VideoProcessorSetOutputColorSpace(&processor, &out_cs);
                }
            }
        }

        let full = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        unsafe {
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&full));
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&full));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full));
        }

        Ok(Self {
            video_device,
            video_context,
            enumerator,
            processor,
            width,
            height,
        })
    }

    /// Convert one frame. `dest` must have the matching output format.
    pub fn convert(&self, src: &ID3D11Texture2D, dest: &ID3D11Texture2D) -> WincapResult<()> {
        let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let mut in_view: Option<ID3D11VideoProcessorInputView> = None;
        hr_call!("video_processor", unsafe {
            self.video_device
                .CreateVideoProcessorInputView(src, &self.enumerator, &in_desc, Some(&mut in_view))
        });
        let in_view = in_view.ok_or_else(|| crate::error::WincapError::General {
            component: "video_processor",
            message: "CreateVideoProcessorInputView returned None".into(),
        })?;

        let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
        hr_call!("video_processor", unsafe {
            self.video_device
                .CreateVideoProcessorOutputView(dest, &self.enumerator, &out_desc, Some(&mut out_view))
        });
        let out_view = out_view.ok_or_else(|| crate::error::WincapError::General {
            component: "video_processor",
            message: "CreateVideoProcessorOutputView returned None".into(),
        })?;

        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            pInputSurface: unsafe { std::mem::transmute_copy(&in_view) },
            ..Default::default()
        };

        hr_call!("video_processor", unsafe {
            self.video_context
                .VideoProcessorBlt(&self.processor, &out_view, 0, &[stream])
        });

        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}
