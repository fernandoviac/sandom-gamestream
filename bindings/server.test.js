// The binding decodes the event records that src/native.rs encodes; the
// Rust test `event_records_match_the_binding_fixture` pins the same bytes.

import { assertEquals, assertThrows } from "@std/assert";
import { decodeEvent, encodeConfiguration } from "./server.js";

const EXPECTED = [
  { kind: "pinRequired", client: "C1" },
  {
    kind: "sessionStarted", width: 1280, height: 720, fps: 60, codec: "h264", bitrate: 10_000_000, maxReferenceFrames: 1,
    fullRange: false,
  },
  { kind: "keyFrameRequested" },
  { kind: "input", input: { type: "keyDown", key: 0x41 } },
  { kind: "input", input: { type: "mouseMoveAbsolute", x: -5, y: 96, screenWidth: 1280, screenHeight: 720 } },
  { kind: "input", input: { type: "mouseMoveRelative", x: -3, y: 4 } },
  { kind: "input", input: { type: "mouseButtonUp", button: "right" } },
  { kind: "input", input: { type: "scrollVertical", amount: -120 } },
  { kind: "input", input: { type: "scrollHorizontal", amount: 240 } },
  { kind: "input", input: { type: "touch", event: "down", pointerId: 2, x: 0.5, y: 0.25 } },
  {
    kind: "input",
    input: { type: "pen", event: "move", tool: "eraser", buttons: 1, x: 0.5, y: 0.75, pressureOrDistance: 0.25, rotation: 90, tilt: 30 },
  },
  { kind: "input", input: { type: "text", text: "hé" } },
  { kind: "sessionEnded" },
];

function bytes(hex) {
  return new Uint8Array(hex.match(/../g).map((pair) => parseInt(pair, 16)));
}

Deno.test("each event record of the server decodes to its event", async () => {
  const fixture = await Deno.readTextFile(new URL("./event-records.hex", import.meta.url));
  const records = fixture.trim().split("\n");
  assertEquals(records.length, EXPECTED.length);
  records.forEach((record, index) => assertEquals(decodeEvent(bytes(record)), EXPECTED[index], record));
});

Deno.test("an unknown event kind names itself", () => {
  assertThrows(() => decodeEvent(new Uint8Array([9])), Error, "event kind 9");
});

Deno.test("the configuration record holds its ports and strings in order", () => {
  const record = encodeConfiguration({
    hostName: "h", certificateDirectory: "/c", applicationTitle: "t", bindAddress: "::", ports: { https: 48984 },
  });
  assertEquals(
    record,
    bytes("01" + "0000" + "58bf" + "0000".repeat(4) + "0100000068" + "020000002f63" + "0100000074" + "020000003a3a"),
  );
  assertThrows(() => encodeConfiguration({ hostName: "h", certificateDirectory: "/c", applicationTitle: "t", bindAddress: "::", ports: { web: 1 } }),
    TypeError, "no port named web");
});
