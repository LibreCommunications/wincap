// Public TypeScript types for @librecommunications/wincap.
// Mirrors pipecap's shape where reasonable.

export type SourceKind = 'display' | 'window';

export interface DisplaySource {
  kind: 'display';
  /** Win32 HMONITOR — pass back when starting a CaptureSession. */
  monitorHandle: bigint;
  name: string;
  primary: boolean;
  bounds: Rect;
}

export interface WindowSource {
  kind: 'window';
  /** Win32 HWND. */
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

export interface CaptureOptions {
  source: { kind: 'display'; monitorHandle: bigint }
        | { kind: 'window';  hwnd: bigint };
  /** Soft FPS cap. 0 = uncapped (vsync-driven). Default: 0. */
  fps?: number;
  includeCursor?: boolean;
  /** Show the WGC yellow border. Win11 22H2+ allows `false`. */
  borderRequired?: boolean;
}

export interface VideoFrame {
  timestampNs: bigint;
  width: number;
  height: number;
  format: PixelFormat;
  sizeChanged: boolean;
  /** MUST be called when the consumer is done with the frame. */
  release(): void;
}

export interface CaptureStats {
  deliveredFrames: bigint;
  droppedFrames: bigint;
}

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
