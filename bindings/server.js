// The GameStream server over a binding (deno.js or node.js), the same under
// Deno, Bun and Node. The server rings its doorbell when events wait; the
// doorbell takes every waiting event record (src/native.rs) and gives each
// to `onEvent` in order. `onEvent` is a callback whose return value and
// Promise the server ignores.

const decoder = new TextDecoder();
const encoder = new TextEncoder();

// Starts a server through `native`, the calls of a binding: `start(config,
// ring)`, `takeEvent(server, buffer)`, `sendAccessUnit(server, data, key,
// timestamp)`, `endSession(server)`, `stop(server)`, `free(server)` and
// `lastError(server)`, and `doorbell(ring)` with `ref`, `unref` and
// `close`.
export function createGameStreamServer(native, config, onEvent) {
  if (typeof onEvent !== "function") throw new TypeError("The GameStream server takes an event callback");
  let server = null;
  let buffer = new Uint8Array(4096);
  let stopped = false;

  function takeEvents() {
    for (;;) {
      const length = native.takeEvent(server, buffer);
      if (length === 0) return;
      if (length < 0) {
        buffer = new Uint8Array(-length);
        continue;
      }
      onEvent(JSON.parse(decoder.decode(buffer.subarray(0, length))));
    }
  }

  // The doorbell may ring before `start` returns the server; the events
  // then wait for the first drain after it.
  let started = false;
  const doorbell = native.doorbell(() => {
    if (started) takeEvents();
  });
  try {
    server = native.start(encoder.encode(JSON.stringify(config)), doorbell);
  } catch (error) {
    doorbell.close();
    throw error;
  }
  started = true;
  // Events from before `start` returned have rung already.
  takeEvents();
  // A running server keeps the process alive, as a listening socket does.
  doorbell.ref();

  function check(status, operation) {
    if (status !== 0) throw new Error(`The GameStream server could not ${operation}: ${native.lastError(server)}`);
  }

  function running(operation) {
    if (stopped) throw new Error(`The GameStream server has stopped; it cannot ${operation}`);
  }

  return Object.freeze({
    // Sends one H.264 access unit (Annex B bytes) of the running session;
    // `timestamp` is its presentation time in microseconds.
    sendAccessUnit(data, { key, timestamp }) {
      running("send an access unit");
      if (!(data instanceof Uint8Array)) throw new TypeError("An access unit is a Uint8Array of Annex B bytes");
      if (!Number.isSafeInteger(timestamp)) throw new TypeError(`An access unit's timestamp is whole microseconds; observed ${timestamp}`);
      check(native.sendAccessUnit(server, data, key ? 1 : 0, timestamp), "send an access unit");
    },
    // Ends the running session; a `sessionEnded` event follows.
    async endSession() {
      running("end the session");
      check(await native.endSession(server), "end the session");
    },
    // Stops the server and closes its ports. Events that arrived before it
    // stopped reach `onEvent` first.
    async stop() {
      running("stop");
      stopped = true;
      check(await native.stop(server), "stop");
      takeEvents();
      doorbell.close();
      check(native.free(server), "free its state");
    },
  });
}
