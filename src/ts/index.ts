// Public facade. Wraps the native CaptureSession + AudioSession in
// EventEmitter / AsyncIterable interfaces.

import { EventEmitter } from 'node:events';
// eslint-disable-next-line @typescript-eslint/no-var-requires
const native: NativeBindings = require('node-gyp-build')(__dirname + '/..');

import type {
  AudioChunk,
  AudioOptions,
  AudioStats,
  Capabilities,
  CaptureOptions,
  CaptureStats,
  DisplaySource,
  EncodedFrame,
  ErrorInfo,
  Source,
  VideoFrame,
  WindowSource,
} from './types';

export * from './types';

interface NativeBindings {
  version(): string;
  listDisplays(): DisplaySource[];
  listWindows(): WindowSource[];
  getCapabilities(): Capabilities;

  CaptureSession: new (
    opts: CaptureOptions,
    onFrame:   (f: VideoFrame) => void,
    onEncoded: (f: EncodedFrame) => void,
    onError:   (e: ErrorInfo) => void,
  ) => NativeCaptureSession;

  AudioSession: new (
    opts: AudioOptions,
    onChunk: (c: AudioChunk) => void,
    onError: (e: ErrorInfo) => void,
  ) => NativeAudioSession;
}

interface NativeCaptureSession {
  start(): void;
  stop(): void;
  getStats(): CaptureStats;
  requestKeyframe(): void;
  setBitrate(bps: number): void;
}

interface NativeAudioSession {
  start(): void;
  stop(): void;
  getStats(): AudioStats;
}

export function version(): string {
  return native.version();
}
export function listDisplays(): DisplaySource[] {
  return native.listDisplays();
}
export function listWindows(): WindowSource[] {
  return native.listWindows();
}
export function listSources(filter?: { displays?: boolean; windows?: boolean }): Source[] {
  const out: Source[] = [];
  if (filter?.displays ?? true) out.push(...native.listDisplays());
  if (filter?.windows  ?? true) out.push(...native.listWindows());
  return out;
}
export function getCapabilities(): Capabilities {
  return native.getCapabilities();
}

// ----- CaptureSession -----

type CaptureEvents = {
  frame:   (f: VideoFrame) => void;
  encoded: (f: EncodedFrame) => void;
  error:   (e: ErrorInfo) => void;
};

export class CaptureSession extends EventEmitter {
  readonly #native: NativeCaptureSession;
  #stopped = false;

  constructor(opts: CaptureOptions) {
    super();
    this.#native = new native.CaptureSession(
      opts,
      (f) => this.emit('frame',   f),
      (f) => this.emit('encoded', f),
      (e) => this.emit('error',   e),
    );
  }

  on<K extends keyof CaptureEvents>(event: K, listener: CaptureEvents[K]): this {
    return super.on(event, listener as (...args: unknown[]) => void);
  }
  emit<K extends keyof CaptureEvents>(event: K, ...args: Parameters<CaptureEvents[K]>): boolean {
    return super.emit(event, ...args);
  }

  start(): void {
    if (this.#stopped) throw new Error('CaptureSession: already stopped');
    this.#native.start();
  }
  stop(): void {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#native.stop();
  }
  getStats(): CaptureStats {
    return this.#native.getStats();
  }
  requestKeyframe(): void {
    this.#native.requestKeyframe();
  }
  setBitrate(bps: number): void {
    this.#native.setBitrate(bps);
  }
}

// ----- AudioSession -----

type AudioEvents = {
  chunk: (c: AudioChunk) => void;
  error: (e: ErrorInfo) => void;
};

export class AudioSession extends EventEmitter {
  readonly #native: NativeAudioSession;
  #stopped = false;

  constructor(opts: AudioOptions) {
    super();
    this.#native = new native.AudioSession(
      opts,
      (c) => this.emit('chunk', c),
      (e) => this.emit('error', e),
    );
  }

  on<K extends keyof AudioEvents>(event: K, listener: AudioEvents[K]): this {
    return super.on(event, listener as (...args: unknown[]) => void);
  }
  emit<K extends keyof AudioEvents>(event: K, ...args: Parameters<AudioEvents[K]>): boolean {
    return super.emit(event, ...args);
  }

  start(): void {
    if (this.#stopped) throw new Error('AudioSession: already stopped');
    this.#native.start();
  }
  stop(): void {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#native.stop();
  }
  getStats(): AudioStats {
    return this.#native.getStats();
  }
}
