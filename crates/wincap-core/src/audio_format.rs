use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};

#[derive(Debug, Clone)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u32,
    pub float32: bool,
    pub bits_per_sample: u32,
}

impl AudioFormat {
    pub fn bytes_per_frame(&self) -> u32 {
        self.channels * (self.bits_per_sample / 8)
    }

    /// Parse an AudioFormat from a WAVEFORMATEX pointer.
    ///
    /// # Safety
    /// The pointer must be valid and point to a complete WAVEFORMATEX
    /// (or WAVEFORMATEXTENSIBLE if wFormatTag == WAVE_FORMAT_EXTENSIBLE).
    pub unsafe fn from_wave_format(wf: *const WAVEFORMATEX) -> Self {
        if wf.is_null() {
            return Self {
                sample_rate: 0,
                channels: 0,
                float32: true,
                bits_per_sample: 32,
            };
        }

        let wf = &*wf;
        let mut format = AudioFormat {
            sample_rate: wf.nSamplesPerSec,
            channels: wf.nChannels as u32,
            bits_per_sample: wf.wBitsPerSample as u32,
            float32: false,
        };

        if wf.wFormatTag == WAVE_FORMAT_IEEE_FLOAT as u16 {
            format.float32 = true;
        } else if wf.wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16 && wf.cbSize >= 22 {
            let ext = wf as *const WAVEFORMATEX as *const WAVEFORMATEXTENSIBLE;
            let ext = &*ext;
            let sub_format = std::ptr::read_unaligned(std::ptr::addr_of!(ext.SubFormat));
            if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                format.float32 = true;
            }
        }

        format
    }
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            float32: true,
            bits_per_sample: 32,
        }
    }
}
