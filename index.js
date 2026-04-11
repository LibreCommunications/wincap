const { EventEmitter } = require('node:events');

// Load the native addon. napi-rs puts the .node file next to this file.
const native = require('./wincap.win32-x64-msvc.node');

// Re-export static functions.
module.exports.version = native.version;
module.exports.listDisplays = native.listDisplays;
module.exports.listWindows = native.listWindows;
module.exports.getCapabilities = native.getCapabilities;

function listSources(filter) {
  const out = [];
  if (!filter || filter.displays !== false) out.push(...native.listDisplays());
  if (!filter || filter.windows !== false) out.push(...native.listWindows());
  return out;
}
module.exports.listSources = listSources;

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
module.exports.CaptureSession = CaptureSession;

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
module.exports.AudioSession = AudioSession;
