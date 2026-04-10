# wincap

Native Windows screen capture with audio for Electron — Windows Graphics Capture (WGC) + Media Foundation hardware encoders + WASAPI loopback, exposed via N-API.

The Windows sibling of [`pipecap`](https://github.com/LibreCommunications/pipecap). Built for [LibreCord](https://github.com/LibreCommunications/Librecord) on Electron 41.

> Status: **alpha**. The native pipeline (WGC → NV12 → MF H.264/HEVC/AV1 → JS) is implemented end-to-end. WASAPI loopback (system + Win11 process loopback) is implemented. The HDR/LTR/intra-refresh paths are wired in the encoder but not yet exercised by an integration test. Expect rough edges; CI builds against the LibreCord Electron pin only.

## What it does that the Chromium built-in does not

| Feature | wincap | Chromium `getDisplayMedia` |
| --- | --- | --- |
| GPU-resident video pipeline (no CPU readback) | ✅ | ❌ — always reads back to CPU |
| Hardware vendor-matched encoder (NVENC / QSV / AMF / Adreno) | ✅ via `MFTEnumEx` + `VEN_XXXX` match | ❌ |
| Low-latency CBR/VBR with no B-frames | ✅ `LowDelayVBR` + `AVLowLatencyMode` + `AVEncMPVDefaultBPictureCount=0` | partial |
| Long-term reference frames (LTR) for RTC packet recovery | ✅ `CODECAPI_AVEncVideoLTRBufferControl` | ❌ |
| Intra refresh (no IDR bitrate spikes) | ✅ | ❌ |
| ROI metadata from dirty rects | ✅ `MFSampleExtension_ROIRectangle` | ❌ |
| Hot bitrate / keyframe control | ✅ `setBitrate(bps)`, `requestKeyframe()` | ❌ |
| HDR10 / Main10 capture & encode | ✅ `R16G16B16A16Float` → P010 → BT.2020 PQ | ❌ |
| WASAPI system loopback | ✅ event-driven, `MMCSS "Pro Audio"` | partial |
| WASAPI per-process loopback (Win11) | ✅ `ActivateAudioInterfaceAsync` + `PROCESS_LOOPBACK` | ❌ |
| Tight A/V sync via shared QPC epoch | ✅ — WGC `SystemRelativeTime` and WASAPI `qpcPosition` are in the same clock | ❌ |
| 3 ms WASAPI period via `IAudioClient3` | ✅ with 20 ms fallback | ❌ |
| CPU readback path for renderers that need raw BGRA | ✅ N-buffered staging textures + zero-copy `ArrayBuffer` | n/a |
| Encoded delivery (`H.264 / HEVC / AV1`) | ✅ all three via async MFT | ❌ |
| Per-frame stats (delivered / dropped / encoded units / discontinuities) | ✅ `getStats()` on each session | ❌ |

## Requirements

- Windows 10 1903+ for WGC. Windows 11 22000+ for per-process audio loopback. Windows 11 22H2+ for `IsBorderRequired = false`. 24H2+ for WGC dirty rects.
- Visual Studio 2022 (Desktop C++) and the Win11 SDK (10.0.26100 or newer).
- Node 24 (or Electron 41.1.1 — that's the LibreCord pin and what CI builds against).

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
  // as encoded video — A/V sync is intrinsic, no rebasing.
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

- `{ type: "raw" }` — metadata only. Delivers a `VideoFrame` with `width / height / timestampNs / format` and a `release()` to recycle the GPU pool slot. Use this when a follow-on stage in the same process consumes the texture directly.
- `{ type: "cpu" }` — CPU-mapped BGRA8 `ArrayBuffer` via N-buffered staging textures. Zero-copy at the JS boundary; the `ArrayBuffer` finalizer unmaps and recycles the slot. Use this for renderers that want to ship pixels themselves (canvas, WebGPU, etc).
- `{ type: "encoded", codec, bitrateBps, fps, keyframeIntervalMs?, hdr10?, ltrCount?, intraRefresh?, intraRefreshPeriod?, roiEnabled? }` — hardware H.264/HEVC/AV1 NAL/OBU units. Vendor MFT is auto-selected by adapter LUID. This is the path real-time transports want.

## Audio modes

`AudioSession` accepts `{ mode: "systemLoopback" }` (whole-device mix, Vista+) or `{ mode: "processLoopback", pid, includeTree? }` (Win11 22000+ per-process or process tree).

Both deliver float32 chunks at the device mix rate (typically 48 kHz stereo). The mode auto-falls-back to `IAudioClient::Initialize` with a 20 ms buffer when `IAudioClient3::InitializeSharedAudioStream` isn't supported (process loopback path doesn't expose AC3).

## Architecture (one-liner)

```
WGC FrameArrived  ──►  D3D11 GPU pool  ──►  ID3D11VideoProcessor (BGRA→NV12 / RGBA16F→P010)
                                                │
                                                ▼
                                          MF async MFT (vendor HW encoder, async events on MF work queue)
                                                │
                                                ▼
                                       N-API ThreadSafeFunction.NonBlockingCall
                                                │
                                                ▼
                                          JS thread: 'encoded' callback
```

```
WASAPI loopback (event-driven, MMCSS "Pro Audio")
   │  IAudioClient3::InitializeSharedAudioStream (3 ms period when supported)
   ▼
SPSC pool of float32 buffers ──► N-API TSFN ──► JS 'chunk' callback
```

Both paths share the same QPC epoch — `WGC SystemRelativeTime` and `WASAPI qpcPosition` are in the same clock. A/V sync is automatic.

## Build / development

```sh
npm install
npx cmake-js compile             # Node ABI
npx cmake-js compile --runtime=electron --runtime-version=41.1.1
npm run build:ts
```

`@librecord/wincap` is **not** published to npm. Distribution is via GitHub Releases on `v*` tags (mirrors pipecap's flow). The CI matrix is intentionally tiny: `windows-2025` + Node 24, Electron 41.1.1, x64. Add architectures here when LibreCord adds them.

## Caveats

- **DRM-protected content** (Netflix, etc) returns black frames from WGC. There is no workaround at the WGC layer.
- **Hybrid GPU laptops**: wincap pins the D3D device to the LUID of the source's output adapter, avoiding the cross-PCIe copy that hurts Discord. Verified on the iGPU/dGPU paths but not on Optimus mux-less.
- **WGC dirty rects** (24H2 only) are plumbed through `CapturedFrame::dirty_rects` but the actual `IGraphicsCaptureSession3::TryGetDirtyRegions` QI is gated until the SDK pin lands. The encoder ROI path consumes them when present.
- **AV1 driver allowlist**: vendor-matched MFT activation prefers the running adapter's encoder, but old Intel/AMD AV1 MFTs deadlock under sustained load. AV1 should be considered experimental until the per-vendor allowlist is hardened.

## License

AGPL-3.0-or-later © LibreCommunications. See [LICENSE](./LICENSE).
