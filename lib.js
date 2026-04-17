const { EventEmitter } = require('node:events');
const native = require('./index');

// Re-export static functions.
exports.version = native.version;

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
