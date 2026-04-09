// Public facade. The native addon exposes a low-level CaptureSession class
// that takes raw onFrame / onError callbacks; we wrap it in an EventEmitter
// + AsyncIterable for ergonomic consumption from Electron.

import { EventEmitter } from 'node:events';
// node-gyp-build resolves the appropriate prebuild for the current
// Node/Electron ABI. The fallback path (build/Release/wincap.node) is
// used during local development.
// eslint-disable-next-line @typescript-eslint/no-var-requires
const native: NativeBindings = require('node-gyp-build')(__dirname + '/..');

import type {
  CaptureOptions,
  CaptureStats,
  Capabilities,
  DisplaySource,
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
    onFrame: (f: VideoFrame) => void,
    onError: (e: ErrorInfo) => void,
  ) => NativeCaptureSession;
}

interface NativeCaptureSession {
  start(): void;
  stop(): void;
  getStats(): CaptureStats;
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
  const wantD = filter?.displays ?? true;
  const wantW = filter?.windows  ?? true;
  const out: Source[] = [];
  if (wantD) out.push(...native.listDisplays());
  if (wantW) out.push(...native.listWindows());
  return out;
}

export function getCapabilities(): Capabilities {
  return native.getCapabilities();
}

type CaptureEvents = {
  frame: (frame: VideoFrame) => void;
  error: (err: ErrorInfo) => void;
};

export class CaptureSession extends EventEmitter {
  readonly #native: NativeCaptureSession;
  #stopped = false;
  #pendingFrames: VideoFrame[] = [];
  #pendingResolvers: Array<(v: IteratorResult<VideoFrame>) => void> = [];

  constructor(opts: CaptureOptions) {
    super();
    this.#native = new native.CaptureSession(
      opts,
      (frame) => this.#onFrame(frame),
      (err)   => this.emit('error', err),
    );
  }

  // Type-safe overloads.
  override on<K extends keyof CaptureEvents>(event: K, listener: CaptureEvents[K]): this {
    return super.on(event, listener as (...args: unknown[]) => void);
  }
  override emit<K extends keyof CaptureEvents>(event: K, ...args: Parameters<CaptureEvents[K]>): boolean {
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
    // Drain any pending iterator consumers.
    while (this.#pendingResolvers.length > 0) {
      const r = this.#pendingResolvers.shift()!;
      r({ value: undefined, done: true });
    }
    // Release any pending frames the consumer hasn't pulled.
    while (this.#pendingFrames.length > 0) {
      this.#pendingFrames.shift()!.release();
    }
  }

  getStats(): CaptureStats {
    return this.#native.getStats();
  }

  #onFrame(frame: VideoFrame): void {
    // Listener path takes precedence; if anyone subscribed to 'frame', they
    // own the lifetime and must call frame.release().
    if (this.listenerCount('frame') > 0) {
      this.emit('frame', frame);
      return;
    }
    // Iterator path.
    if (this.#pendingResolvers.length > 0) {
      const r = this.#pendingResolvers.shift()!;
      r({ value: frame, done: false });
      return;
    }
    // No consumers attached: drop oldest to bound memory.
    if (this.#pendingFrames.length >= 2) {
      this.#pendingFrames.shift()!.release();
    }
    this.#pendingFrames.push(frame);
  }

  [Symbol.asyncIterator](): AsyncIterator<VideoFrame> {
    return {
      next: (): Promise<IteratorResult<VideoFrame>> => {
        if (this.#pendingFrames.length > 0) {
          return Promise.resolve({ value: this.#pendingFrames.shift()!, done: false });
        }
        if (this.#stopped) {
          return Promise.resolve({ value: undefined, done: true });
        }
        return new Promise((resolve) => {
          this.#pendingResolvers.push(resolve);
        });
      },
      return: async (): Promise<IteratorResult<VideoFrame>> => {
        this.stop();
        return { value: undefined, done: true };
      },
    };
  }
}
