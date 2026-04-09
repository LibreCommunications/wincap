# wincap

Native Windows screen capture with audio for Electron — Windows Graphics Capture (WGC) + WASAPI loopback, exposed via N-API.

The Windows sibling of [`pipecap`](https://github.com/LibreCommunications/pipecap). Built for [LibreCord](https://github.com/LibreCommunications) on Electron 41.

> Status: **M1 in progress** — WGC video path scaffolded, audio + encoder coming. Not yet shippable.

## Goals

- GPU-resident, sub-frame latency video capture (WGC primary, DXGI Desktop Duplication fallback).
- WASAPI system + per-process loopback audio (Win11).
- Shared QPC clock for A/V sync.
- Zero-copy delivery: external `ArrayBuffer`, shared NT D3D11 textures, or hardware-encoded H.264/HEVC/AV1 via Media Foundation.
- Prebuilds for Node 20/22 + Electron 34/41 × x64/arm64.

## Quick start (once prebuilds land)

```ts
import { CaptureSession, listDisplays, getCapabilities } from '@librecommunications/wincap';

console.log(getCapabilities());

const [primary] = listDisplays();
const session = new CaptureSession({
  source: { kind: 'display', monitorHandle: primary.monitorHandle },
  includeCursor: true,
  borderRequired: false,
});

session.on('frame', (f) => {
  console.log(f.width, f.height, f.timestampNs);
  f.release();
});

session.on('error', console.error);
session.start();
```

## Building locally

Requires Visual Studio 2022 (Desktop C++) and Windows 11 SDK 10.0.22621+.

```sh
npm install
npx cmake-js compile
npm run build:ts
```

## License

MIT © LibreCommunications
