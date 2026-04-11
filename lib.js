const { EventEmitter } = require('node:events');
const native = require('./index');

// Re-export static functions.
exports.version = native.version;
exports.listDisplays = native.listDisplays;
exports.listWindows = native.listWindows;
exports.getCapabilities = native.getCapabilities;

exports.listSources = function listSources(filter) {
  const out = [];
  if (!filter || filter.displays !== false) out.push(...native.listDisplays());
  if (!filter || filter.windows !== false) out.push(...native.listWindows());
  return out;
};

// ----- CaptureSession -----

class CaptureSession extends EventEmitter {
  #native;
  #stopped = false;

  constructor(opts) {
    super();
    this.#native = new native.CaptureSession(
      opts,
      (f) => this.emit('frame', f),
      (f) => this.emit('encoded', f),
      (e) => this.emit('error', e),
    );
  }

  start() {
    if (this.#stopped) throw new Error('CaptureSession: already stopped');
    this.#native.start();
  }

  stop() {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#native.stop();
  }

  getStats() {
    return this.#native.getStats();
  }

  requestKeyframe() {
    this.#native.requestKeyframe();
  }

  setBitrate(bps) {
    this.#native.setBitrate(bps);
  }
}
exports.CaptureSession = CaptureSession;

// ----- AudioSession -----

class AudioSession extends EventEmitter {
  #native;
  #stopped = false;

  constructor(opts) {
    super();
    this.#native = new native.AudioSession(
      opts,
      (c) => this.emit('chunk', c),
      (e) => this.emit('error', e),
    );
  }

  start() {
    if (this.#stopped) throw new Error('AudioSession: already stopped');
    this.#native.start();
  }

  stop() {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#native.stop();
  }

  getStats() {
    return this.#native.getStats();
  }
}
exports.AudioSession = AudioSession;
