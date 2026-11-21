// The Node and Bun binding of the GameStream server: the Node-API addon of
// node/, which links the library and forwards each call to its C interface.

import { existsSync } from "node:fs";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { checkoutLibrary, installedLibrary, nodeAddon } from "./library.js";
import { createGameStreamServer } from "./server.js";

// The addon of this platform's installed package, else the one built in
// this repository's checkout, else null.
export function gameStreamLibrary() {
  const installed = installedLibrary();
  if (installed !== null) return nodeAddon(installed);
  const library = checkoutLibrary();
  if (library === null) return null;
  const addon = nodeAddon(library);
  return existsSync(addon) ? addon : null;
}

// Bun 1.3.14's Node-API hands the addon the inline storage of a small typed
// array that JavaScriptCore has not yet moved into an ArrayBuffer, and then
// moves it, so the addon reads or writes storage the array no longer uses
// (sandom-gpu-host bindings/node.js). Reading `buffer` moves the array into
// its ArrayBuffer first, so each call reads it for every typed-array
// argument, such as an access unit of a few hundred bytes; under Node the
// read changes nothing.
function materializingTypedArrays(exports) {
  return Object.fromEntries(Object.entries(exports).map(([name, call]) => [name, (...values) => {
    for (const value of values) if (ArrayBuffer.isView(value)) void value.buffer;
    return call(...values);
  }]));
}

// The exports of the addon at each path, loaded once.
const addons = new Map();
function loadAddon(addonPath) {
  if (addonPath === null) {
    throw new Error("The GameStream server has no Node addon for this platform; build it with `deno task build`");
  }
  const path = addonPath instanceof URL ? fileURLToPath(addonPath) : addonPath;
  let exports = addons.get(path);
  if (exports === undefined) {
    const addon = { exports: {} };
    process.dlopen(addon, path);
    exports = materializingTypedArrays(addon.exports);
    addons.set(path, exports);
  }
  return exports;
}

// Starts a GameStream server from the addon at `addonPath` (a path or a
// file URL). See server.js for `config` and `onEvent`.
export function startGameStreamServer(config, onEvent, addonPath = gameStreamLibrary()) {
  const native = loadAddon(addonPath);
  return createGameStreamServer({
    doorbell(ring) {
      const handle = native.doorbell(ring);
      return {
        handle,
        ref: () => native.doorbellRef(handle),
        unref: () => native.doorbellUnref(handle),
        close: () => native.doorbellClose(handle),
      };
    },
    start: (config, doorbell) => native.start(config, doorbell.handle),
    takeEvent: (server, buffer) => native.takeEvent(server, buffer),
    sendAccessUnit: (server, data, key, timestamp) => native.sendAccessUnit(server, data, key, timestamp),
    // The server's doorbell never waits for this thread, so both wait here.
    endSession: (server) => native.endSession(server),
    stop: (server) => native.stop(server),
    free: (server) => native.free(server),
    lastError: (server) => native.lastError(server),
  }, config, onEvent);
}
