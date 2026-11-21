// The Deno binding of the GameStream server: the library through Deno's FFI
// (`--allow-ffi`, and `--allow-read` for the library path).

import { checkoutLibrary, installedLibrary } from "./library.js";
import { createGameStreamServer } from "./server.js";

const SYMBOLS = {
  sandom_gamestream_start: { parameters: ["buffer", "usize", "function", "pointer", "buffer", "usize", "buffer"], result: "pointer" },
  sandom_gamestream_take_event: { parameters: ["pointer", "buffer", "usize"], result: "i64" },
  sandom_gamestream_send_access_unit: { parameters: ["pointer", "buffer", "usize", "u32", "i64"], result: "i32" },
  // Both wait for the server's tasks, which may wait for the doorbell on
  // this thread; they therefore run off this thread.
  sandom_gamestream_end_session: { parameters: ["pointer"], result: "i32", nonblocking: true },
  sandom_gamestream_stop: { parameters: ["pointer"], result: "i32", nonblocking: true },
  sandom_gamestream_free: { parameters: ["pointer"], result: "i32" },
  sandom_gamestream_last_error: { parameters: ["pointer", "buffer", "usize"], result: "usize" },
};
const decoder = new TextDecoder();

// The library of this platform's installed package, else the one built in
// this repository's checkout, else null.
export function gameStreamLibrary() {
  const installed = installedLibrary();
  if (installed !== null) return installed;
  const library = checkoutLibrary();
  if (library === null) return null;
  try {
    Deno.statSync(library);
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return null;
    throw error;
  }
  return library;
}

// Starts a GameStream server from the library at `libraryPath` (a path or a
// file URL). See server.js for `config` and `onEvent`.
export function startGameStreamServer(config, onEvent, libraryPath = gameStreamLibrary()) {
  if (libraryPath === null) {
    throw new Error("The GameStream server has no library for this platform; build it with `deno task build`");
  }
  const library = Deno.dlopen(libraryPath, SYMBOLS);
  const native = library.symbols;
  const server = createGameStreamServer({
    doorbell(ring) {
      const callback = Deno.UnsafeCallback.threadSafe({ parameters: ["pointer"], result: "void" }, () => ring());
      callback.unref();
      return { pointer: callback.pointer, ref: () => callback.ref(), unref: () => callback.unref(), close: () => callback.close() };
    },
    start(config, doorbell) {
      const errorBytes = new Uint8Array(4096);
      const errorLength = new BigUint64Array(1);
      const server = native.sandom_gamestream_start(config, config.byteLength, doorbell.pointer, null, errorBytes,
        errorBytes.length, new Uint8Array(errorLength.buffer));
      if (server === null) {
        const length = Math.min(Number(errorLength[0]), errorBytes.length);
        throw new Error(`The GameStream server did not start: ${decoder.decode(errorBytes.subarray(0, length))}`);
      }
      return server;
    },
    takeEvent: (server, buffer) => Number(native.sandom_gamestream_take_event(server, buffer, buffer.byteLength)),
    sendAccessUnit: (server, data, key, timestamp) =>
      native.sandom_gamestream_send_access_unit(server, data, data.byteLength, key, BigInt(timestamp)),
    endSession: (server) => native.sandom_gamestream_end_session(server),
    stop: (server) => native.sandom_gamestream_stop(server),
    free(server) {
      const status = native.sandom_gamestream_free(server);
      if (status === 0) library.close();
      return status;
    },
    lastError(server) {
      const length = Number(native.sandom_gamestream_last_error(server, null, 0));
      const bytes = new Uint8Array(length);
      native.sandom_gamestream_last_error(server, bytes, bytes.length);
      return decoder.decode(bytes);
    },
  }, config, onEvent);
  return server;
}
