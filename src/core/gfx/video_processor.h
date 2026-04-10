// Hardware BGRA → NV12 (or P010 for HDR) color conversion via the D3D11
// Video Processor. This is faster and simpler than a hand-written compute
// shader, runs entirely on the GPU's video block, and avoids the dx
// shader-blob bookkeeping. Created lazily once we know the input size.
//
// Each Convert() call is queued on the immediate context — no fence is
// inserted here; the encoder pipeline owns synchronisation.
#pragma once

#include <wrl/client.h>

#include <d3d11_4.h>

namespace wincap {

class VideoProcessor {
public:
    VideoProcessor() = default;
    ~VideoProcessor() = default;

    VideoProcessor(const VideoProcessor&) = delete;
    VideoProcessor& operator=(const VideoProcessor&) = delete;

    // Initialise for a specific input → output size + format pair.
    // `output_format` is typically DXGI_FORMAT_NV12 (SDR) or P010 (HDR).
    void Init(ID3D11Device5*        device,
              ID3D11DeviceContext4* context,
              UINT                  width,
              UINT                  height,
              DXGI_FORMAT           output_format);

    // Convert one frame. `dest` must be a texture created with the
    // matching format and BIND_RENDER_TARGET. The video processor
    // creates per-call output views; the input view is cached.
    void Convert(ID3D11Texture2D* src, ID3D11Texture2D* dest);

    void Reset() noexcept;

    UINT Width()  const noexcept { return width_; }
    UINT Height() const noexcept { return height_; }

private:
    Microsoft::WRL::ComPtr<ID3D11VideoDevice>             video_device_;
    Microsoft::WRL::ComPtr<ID3D11VideoContext1>           video_context_;
    Microsoft::WRL::ComPtr<ID3D11VideoProcessorEnumerator> enumerator_;
    Microsoft::WRL::ComPtr<ID3D11VideoProcessor>          processor_;

    UINT       width_{0};
    UINT       height_{0};
    DXGI_FORMAT output_format_{DXGI_FORMAT_NV12};
};

} // namespace wincap
