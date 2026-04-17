import { EventEmitter } from 'node:events';

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
  data: Buffer;
}

export interface AudioStats {
  deliveredChunks: bigint;
  droppedChunks: bigint;
  discontinuities: bigint;
}

export interface ErrorInfo {
  component: string;
  hresult: number;
  message: string;
}

export function version(): string;

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
