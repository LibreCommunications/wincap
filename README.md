# wincap

Native Windows screen capture with audio for Electron — Windows Graphics Capture (WGC) + Media Foundation hardware encoders + WASAPI loopback, exposed via napi-rs.

The Windows sibling of [`pipecap`](https://github.com/LibreCommunications/pipecap). Built for [LibreCord](https://github.com/LibreCommunications/Librecord) on Electron 41.

> Status: **alpha**. The native pipeline (WGC -> NV12 -> MF H.264/HEVC/AV1 -> JS) is implemented end-to-end. WASAPI loopback (system + Win11 process loopback) is implemented. The HDR/LTR/intra-refresh paths are wired in the encoder but not yet exercised by an integration test. Expect rough edges; CI builds against the LibreCord Electron pin only.

## What it does that the Chromium built-in does not

| Feature | wincap | Chromium `getDisplayMedia` |
| --- | --- | --- |
| GPU-resident video pipeline (no CPU readback) | yes | no -- always reads back to CPU |
| Hardware vendor-matched encoder (NVENC / QSV / AMF / Adreno) | yes via `MFTEnumEx` + `VEN_XXXX` match | no |
| Low-latency CBR/VBR with no B-frames | yes `LowDelayVBR` + `AVLowLatencyMode` + `AVEncMPVDefaultBPictureCount=0` | partial |
| Long-term reference frames (LTR) for RTC packet recovery | yes `CODECAPI_AVEncVideoLTRBufferControl` | no |
| Intra refresh (no IDR bitrate spikes) | yes | no |
| ROI metadata from dirty rects | yes `MFSampleExtension_ROIRectangle` | no |
| Hot bitrate / keyframe control | yes `setBitrate(bps)`, `requestKeyframe()` | no |
| HDR10 / Main10 capture & encode | yes `R16G16B16A16Float` -> P010 -> BT.2020 PQ | no |
| WASAPI system loopback | yes event-driven, `MMCSS "Pro Audio"` | partial |
| WASAPI per-process loopback (Win11) | yes `ActivateAudioInterfaceAsync` + `PROCESS_LOOPBACK` | no |
| Tight A/V sync via shared QPC epoch | yes -- WGC `SystemRelativeTime` and WASAPI `qpcPosition` are in the same clock | no |
| 3 ms WASAPI period via `IAudioClient3` | yes with 20 ms fallback | no |
| CPU readback path for renderers that need raw BGRA | yes N-buffered staging textures + zero-copy `ArrayBuffer` | n/a |
| Encoded delivery (`H.264 / HEVC / AV1`) | yes all three via async MFT | no |
| Per-frame stats (delivered / dropped / encoded units / discontinuities) | yes `getStats()` on each session | no |

## Requirements

- Windows 10 1903+ for WGC. Windows 11 22000+ for per-process audio loopback. Windows 11 22H2+ for `IsBorderRequired = false`. 24H2+ for WGC dirty rects.
- Rust stable (MSVC toolchain) and the Win11 SDK (10.0.26100 or newer).
- Node 18+ (or Electron 41.1.1 -- that's the LibreCord pin and what CI builds against).

## Quick start

```ts
import { CaptureSession, AudioSession, listDisplays, getCapabilities } from "@librecord/wincap";

console.log(getCapabilities());
// { wgc: true, wgcBorderOptional: true, processLoopback: true, windowsBuild: 26100 }

const [primary] = listDisplays();

// 1. Encoded video (low-latency H.264, hardware encoder)
const session = new CaptureSession({
  source: { kind: "display", monitorHandle: primary.monitorHandle },
  delivery: {
    type: "encoded",
    codec: "h264",
    bitrateBps: 8_000_000,
    fps: 60,
    keyframeIntervalMs: 2000,
    ltrCount: 4,
    intraRefresh: true,
  },
  includeCursor: true,
  borderRequired: false,
});

session.on("encoded", (au) => {
  // au.data is an ArrayBuffer of one or more concatenated NAL/OBU units.
  // au.timestampNs is QPC ns since process start (matches AudioSession).
  socket.send(au.data);
});
session.on("error", console.error);
session.start();

// 2. System loopback audio
const audio = new AudioSession({ mode: "systemLoopback" });
audio.on("chunk", (c) => {
  // c.data is interleaved float32, c.timestampNs in the SAME QPC epoch
  // as encoded video -- A/V sync is intrinsic, no rebasing.
  encoder.write(c.data, Number(c.timestampNs));
});
audio.start();

// Bandwidth-estimator integration:
session.setBitrate(4_000_000);
session.requestKeyframe();

// Stats:
console.log(session.getStats());
// { deliveredFrames, droppedFrames, encodedUnits }
console.log(audio.getStats());
// { deliveredChunks, droppedChunks, discontinuities }
```

## Delivery modes

`CaptureOptions.delivery` selects the output shape:

- `{ type: "raw" }` -- metadata only. Delivers a `VideoFrame` with `width / height / timestampNs / format` and a `release()` to recycle the GPU pool slot. Use this when a follow-on stage in the same process consumes the texture directly.
- `{ type: "cpu" }` -- CPU-mapped BGRA8 `ArrayBuffer` via N-buffered staging textures. Zero-copy at the JS boundary; the `ArrayBuffer` finalizer unmaps and recycles the slot. Use this for renderers that want to ship pixels themselves (canvas, WebGPU, etc).
- `{ type: "encoded", codec, bitrateBps, fps, keyframeIntervalMs?, hdr10?, ltrCount?, intraRefresh?, intraRefreshPeriod?, roiEnabled? }` -- hardware H.264/HEVC/AV1 NAL/OBU units. Vendor MFT is auto-selected by adapter LUID. This is the path real-time transports want.

## Audio modes

`AudioSession` accepts `{ mode: "systemLoopback" }` (whole-device mix, Vista+) or `{ mode: "processLoopback", pid, includeTree? }` (Win11 22000+ per-process or process tree).

Both deliver float32 chunks at the device mix rate (typically 48 kHz stereo). The mode auto-falls-back to `IAudioClient::Initialize` with a 20 ms buffer when `IAudioClient3::InitializeSharedAudioStream` isn't supported (process loopback path doesn't expose AC3).

## Architecture

```
WGC FrameArrived  -->  D3D11 GPU pool  -->  ID3D11VideoProcessor (BGRA->NV12 / RGBA16F->P010)
                                                |
                                                v
                                          MF async MFT (vendor HW encoder, async events on MF work queue)
                                                |
                                                v
                                       napi-rs ThreadsafeFunction
                                                |
                                                v
                                          JS thread: 'encoded' callback
```

```
WASAPI loopback (event-driven, MMCSS "Pro Audio")
   |  IAudioClient3::InitializeSharedAudioStream (3 ms period when supported)
   v
SPSC pool of float32 buffers --> napi-rs TSFN --> JS 'chunk' callback
```

Both paths share the same QPC epoch -- `WGC SystemRelativeTime` and `WASAPI qpcPosition` are in the same clock. A/V sync is automatic.

## Build / development

```sh
# Install Rust (MSVC toolchain) if not already installed:
rustup default stable-x86_64-pc-windows-msvc

# Install npm dependencies and build:
npm install
npm run build          # release build
npm run build:debug    # debug build
```

The build produces `wincap.win32-x64-msvc.node` in the project root.

### Project structure

```
wincap/
  Cargo.toml                  workspace root
  index.js                    EventEmitter facade
  index.d.ts                  TypeScript types
  crates/
    wincap-core/              pure Rust, no Node dependency
      src/
        error.rs              Result-based error handling
        clock.rs              QPC clock
        mmcss.rs              RAII MMCSS thread priority
        spsc_ring.rs          lock-free SPSC ring buffer
        frame_pool.rs         lock-free GPU texture pool
        d3d_device.rs         D3D11 device + WinRT projection
        video_processor.rs    BGRA->NV12/P010 color conversion
        wgc_source.rs         Windows.Graphics.Capture
        audio_format.rs       WAVEFORMATEX helpers
        wasapi_loopback.rs    WASAPI loopback capture
        mf_encoder.rs         Media Foundation async HW encoder
    wincap-napi/              napi-rs bindings
      src/
        lib.rs                module exports
        sources.rs            display/window enumeration
        capture_session.rs    CaptureSession JS class
        audio_session.rs      AudioSession JS class
```

`@librecord/wincap` is **not** published to npm. Distribution is via GitHub Releases on `v*` tags (mirrors pipecap's flow). The CI matrix is intentionally tiny: `windows-2025` + Node 24, Electron 41.1.1, x64. Add architectures here when LibreCord adds them.

## Caveats

- **DRM-protected content** (Netflix, etc) returns black frames from WGC. There is no workaround at the WGC layer.
- **Hybrid GPU laptops**: wincap pins the D3D device to the LUID of the source's output adapter, avoiding the cross-PCIe copy that hurts Discord. Verified on the iGPU/dGPU paths but not on Optimus mux-less.
- **WGC dirty rects** (24H2 only) are plumbed through `CapturedFrame::dirty_rects` but the actual `IGraphicsCaptureSession3::TryGetDirtyRegions` QI is gated until the SDK pin lands. The encoder ROI path consumes them when present.
- **AV1 driver allowlist**: vendor-matched MFT activation prefers the running adapter's encoder, but old Intel/AMD AV1 MFTs deadlock under sustained load. AV1 should be considered experimental until the per-vendor allowlist is hardened.

## License

AGPL-3.0-or-later. See [LICENSE](./LICENSE).
