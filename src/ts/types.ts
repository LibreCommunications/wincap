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
  | {
      type: 'encoded';
      codec?: VideoCodec;
      bitrateBps?: number;
      fps?: number;
      keyframeIntervalMs?: number;
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
  sizeChanged: boolean;
  release(): void;
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
