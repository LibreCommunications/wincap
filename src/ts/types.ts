// Public TypeScript types for @librecommunications/wincap.

export type SourceKind = 'display' | 'window';

export interface DisplaySource {
  kind: 'display';
  monitorHandle: bigint;
  name: string;
  primary: boolean;
  bounds: Rect;
}

export interface WindowSource {
  kind: 'window';
  hwnd: bigint;
  title: string;
  pid: number;
  bounds: Rect;
}

export type Source = DisplaySource | WindowSource;

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type PixelFormat = 'bgra8' | 'nv12' | 'p010';
export type VideoCodec = 'h264' | 'hevc' | 'av1';

export type DeliveryMode =
  | { type: 'raw' }
  | { type: 'cpu' }
  | {
      type: 'encoded';
      codec?: VideoCodec;
      bitrateBps?: number;
      fps?: number;
      keyframeIntervalMs?: number;
      /** HDR10 / Main10 — requires HEVC or AV1. */
      hdr10?: boolean;
      /** Long-term reference frame buffer count (RTC packet-loss recovery). */
      ltrCount?: number;
      /** Spread I-block coverage instead of emitting full IDRs. */
      intraRefresh?: boolean;
      intraRefreshPeriod?: number;
      /** Enable per-frame ROI metadata (dirty-rect quality boost). */
      roiEnabled?: boolean;
    };

export interface CaptureOptions {
  source:
    | { kind: 'display'; monitorHandle: bigint }
    | { kind: 'window'; hwnd: bigint };
  delivery?: DeliveryMode;
  fps?: number;
  includeCursor?: boolean;
  borderRequired?: boolean;
}

export interface VideoFrame {
  timestampNs: bigint;
  width: number;
  height: number;
  format: PixelFormat;
  sizeChanged?: boolean;
  /** Row pitch in bytes (CPU delivery only). */
  stride?: number;
  /** Pixel data (CPU delivery only). Backed by mapped staging memory —
   *  do NOT retain past listener invocation; let the ArrayBuffer GC. */
  data?: ArrayBuffer;
  /** Metadata-only delivery exposes a release() to recycle the slot. */
  release?: () => void;
  /** Dirty regions reported by the source (24H2+), if any. */
  dirtyRects?: ReadonlyArray<Rect>;
}

export interface EncodedFrame {
  /** One or more concatenated NAL units. */
  data: ArrayBuffer;
  timestampNs: bigint;
  keyframe: boolean;
}

export interface CaptureStats {
  deliveredFrames: bigint;
  droppedFrames: bigint;
  encodedUnits: bigint;
}

// ----- Audio -----

export type AudioMode =
  | { mode: 'systemLoopback' }
  | { mode: 'processLoopback'; pid: number; includeTree?: boolean };

export type AudioOptions = AudioMode;

export interface AudioChunk {
  timestampNs: bigint;
  frameCount: number;
  sampleRate: number;
  channels: number;
  format: 'float32';
  silent: boolean;
  discontinuity: boolean;
  /** Interleaved float32 samples. Backed by the native pool — copy if
   *  you need to keep it past the listener invocation. */
  data: ArrayBuffer;
}

export interface AudioStats {
  deliveredChunks: bigint;
  droppedChunks: bigint;
  discontinuities: bigint;
}

// ----- Capabilities / errors -----

export interface Capabilities {
  wgc: boolean;
  wgcBorderOptional: boolean;
  processLoopback: boolean;
  windowsBuild: number;
}

export interface ErrorInfo {
  component: string;
  hresult: number;
  message: string;
}
