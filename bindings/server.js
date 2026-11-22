// The GameStream server over a binding (deno.js or node.js), the same under
// Deno, Bun and Node. The server rings its doorbell when events wait; the
// doorbell takes every waiting event record (src/native.rs) and gives each
// to `onEvent` in order. `onEvent` is a callback whose return value and
// Promise the server ignores.

const decoder = new TextDecoder("utf-8", { fatal: true });
const encoder = new TextEncoder();

// The binary records of src/native.rs, whose module documentation defines
// them.
const CONFIGURATION_VERSION = 1;
const PORT_NAMES = ["http", "https", "rtsp", "video", "control", "audio"];
const EVENT_PIN_REQUIRED = 1;
const EVENT_SESSION_STARTED = 2;
const EVENT_KEY_FRAME_REQUESTED = 3;
const EVENT_INPUT = 4;
const EVENT_SESSION_ENDED = 5;
const CODEC_H264 = 1;
const MOUSE_BUTTONS = [null, "left", "middle", "right", "side", "extra"];
const POINTER_EVENTS = ["hover", "down", "up", "move", "cancel", "buttonOnly", "hoverLeave", "cancelAll"];
const PEN_TOOLS = ["unknown", "pen", "eraser"];

// The configuration record of `config`: `hostName`, `certificateDirectory`,
// `applicationTitle`, `bindAddress` and optional `ports` (`http`, `https`,
// `rtsp`, `video`, `control`, `audio`).
export function encodeConfiguration(config) {
  const ports = config.ports ?? {};
  for (const name of Object.keys(ports)) {
    if (!PORT_NAMES.includes(name)) throw new TypeError(`The GameStream server has no port named ${name}`);
  }
  const strings = ["hostName", "certificateDirectory", "applicationTitle", "bindAddress"].map((name) => {
    if (typeof config[name] !== "string") throw new TypeError(`The GameStream server's ${name} is a string; observed ${config[name]}`);
    return encoder.encode(config[name]);
  });
  const record = new Uint8Array(1 + 2 * PORT_NAMES.length + strings.reduce((sum, bytes) => sum + 4 + bytes.length, 0));
  const view = new DataView(record.buffer);
  record[0] = CONFIGURATION_VERSION;
  let offset = 1;
  for (const name of PORT_NAMES) {
    const port = ports[name] ?? 0;
    if (!Number.isInteger(port) || port < 0 || port > 65535) {
      throw new RangeError(`The GameStream server's ${name} port is 1 to 65535, or 0 for the default; observed ${port}`);
    }
    view.setUint16(offset, port, true);
    offset += 2;
  }
  for (const bytes of strings) {
    view.setUint32(offset, bytes.length, true);
    record.set(bytes, offset + 4);
    offset += 4 + bytes.length;
  }
  return record;
}

function named(names, value, what) {
  const name = names[value];
  if (name == null) throw new Error(`The GameStream server sent ${what} ${value}, which this binding does not know`);
  return name;
}

// The input record of the protocol's input event `bytes`: its type (u32 LE)
// and payload (sandom_gamestream_protocol::input).
function decodeInput(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const type = view.getUint32(0, true);
  switch (type) {
    case 0x03:
    case 0x04:
      return { type: type === 0x03 ? "keyDown" : "keyUp", key: view.getUint16(5, true) & 0xff };
    case 0x05:
      return {
        type: "mouseMoveAbsolute", x: view.getInt16(4), y: view.getInt16(6),
        screenWidth: view.getInt16(10), screenHeight: view.getInt16(12),
      };
    case 0x07:
      return { type: "mouseMoveRelative", x: view.getInt16(4), y: view.getInt16(6) };
    case 0x08:
    case 0x09:
      return { type: type === 0x08 ? "mouseButtonDown" : "mouseButtonUp", button: named(MOUSE_BUTTONS, bytes[4], "mouse button") };
    case 0x0a:
      return { type: "scrollVertical", amount: view.getInt16(4) };
    case 0x55000001:
      return { type: "scrollHorizontal", amount: view.getInt16(4) };
    case 0x55000002:
      return {
        type: "touch", event: named(POINTER_EVENTS, bytes[4], "pointer event"), pointerId: view.getUint32(8, true),
        x: view.getFloat32(12, true), y: view.getFloat32(16, true),
      };
    case 0x55000003:
      return {
        type: "pen", event: named(POINTER_EVENTS, bytes[4], "pointer event"), tool: named(PEN_TOOLS, bytes[5], "pen tool"),
        buttons: bytes[6], x: view.getFloat32(8, true), y: view.getFloat32(12, true),
        pressureOrDistance: view.getFloat32(16, true), rotation: view.getUint16(20, true), tilt: bytes[22],
      };
    case 0x17:
      return { type: "text", text: decoder.decode(bytes.subarray(4)) };
    default:
      throw new Error(`The GameStream server sent input event type 0x${type.toString(16)}, which this binding does not know`);
  }
}

// The event of the record `bytes` (see src/native.rs).
export function decodeEvent(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  switch (bytes[0]) {
    case EVENT_PIN_REQUIRED:
      return { kind: "pinRequired", client: decoder.decode(bytes.subarray(5, 5 + view.getUint32(1, true))) };
    case EVENT_SESSION_STARTED:
      if (bytes[13] !== CODEC_H264) throw new Error(`The GameStream server started a session with codec ${bytes[13]}`);
      return {
        kind: "sessionStarted", width: view.getUint32(1, true), height: view.getUint32(5, true), fps: view.getUint32(9, true),
        codec: "h264", bitrate: Number(view.getBigUint64(14, true)), maxReferenceFrames: view.getUint32(22, true),
        fullRange: bytes[26] === 1,
      };
    case EVENT_KEY_FRAME_REQUESTED:
      return { kind: "keyFrameRequested" };
    case EVENT_INPUT:
      return { kind: "input", input: decodeInput(bytes.subarray(1)) };
    case EVENT_SESSION_ENDED:
      return { kind: "sessionEnded" };
    default:
      throw new Error(`The GameStream server sent event kind ${bytes[0]}, which this binding does not know`);
  }
}

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
      onEvent(decodeEvent(buffer.subarray(0, length)));
    }
  }

  // The doorbell may ring before `start` returns the server; the events
  // then wait for the first drain after it.
  let started = false;
  const doorbell = native.doorbell(() => {
    if (started) takeEvents();
  });
  try {
    server = native.start(encodeConfiguration(config), doorbell);
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
