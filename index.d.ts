import { EventEmitter } from 'node:events';

export type SourceKind = 'display' | 'window';

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

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
      hdr10?: boolean;
      ltrCount?: number;
      intraRefresh?: boolean;
      intraRefreshPeriod?: number;
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
  stride?: number;
  data?: ArrayBuffer;
  release?: () => void;
  dirtyRects?: ReadonlyArray<Rect>;
}

export interface EncodedFrame {
  data: ArrayBuffer;
  timestampNs: bigint;
  keyframe: boolean;
}

export interface CaptureStats {
  deliveredFrames: bigint;
  droppedFrames: bigint;
  encodedUnits: bigint;
}

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
  data: ArrayBuffer;
}

export interface AudioStats {
  deliveredChunks: bigint;
  droppedChunks: bigint;
  discontinuities: bigint;
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

// ----- Functions -----

export function version(): string;
export function listDisplays(): DisplaySource[];
export function listWindows(): WindowSource[];
export function listSources(filter?: { displays?: boolean; windows?: boolean }): Source[];
export function getCapabilities(): Capabilities;

// ----- CaptureSession -----

type CaptureEvents = {
  frame: (f: VideoFrame) => void;
  encoded: (f: EncodedFrame) => void;
  error: (e: ErrorInfo) => void;
};

export class CaptureSession extends EventEmitter {
  constructor(opts: CaptureOptions);
  on<K extends keyof CaptureEvents>(event: K, listener: CaptureEvents[K]): this;
  emit<K extends keyof CaptureEvents>(event: K, ...args: Parameters<CaptureEvents[K]>): boolean;
  start(): void;
  stop(): void;
  getStats(): CaptureStats;
  requestKeyframe(): void;
  setBitrate(bps: number): void;
}

// ----- AudioSession -----

type AudioEvents = {
  chunk: (c: AudioChunk) => void;
  error: (e: ErrorInfo) => void;
};

export class AudioSession extends EventEmitter {
  constructor(opts: AudioOptions);
  on<K extends keyof AudioEvents>(event: K, listener: AudioEvents[K]): this;
  emit<K extends keyof AudioEvents>(event: K, ...args: Parameters<AudioEvents[K]>): boolean;
  start(): void;
  stop(): void;
  getStats(): AudioStats;
}
