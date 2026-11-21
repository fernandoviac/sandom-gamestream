// The binding's acceptance (slice 3 of plans/PLAN-gamestream-runtimes.md),
// the same program under Deno, Bun and Node: it starts the server through
// bindings/mod.js on free local ports, reads /serverinfo, receives
// `pinRequired` for the first phase of pairing, stops the server, and then
// finds the pending pairing request answered and every TCP port closed. The
// process then exits by itself, which shows that the server left no
// reference or thread that keeps it alive.
//
//   deno run --allow-ffi --allow-read --allow-write --allow-net --allow-env tools/binding-proof.js
//   bun tools/binding-proof.js
//   node tools/binding-proof.js

import { mkdtempSync, rmSync } from "node:fs";
import { createServer, connect } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gameStreamRuntime, startGameStreamServer } from "../bindings/mod.js";

const HOST = "127.0.0.1";
const CLIENT = "0123456789ABCDEF";
const HOST_NAME = "sandom-binding-proof";

function freePort() {
  return new Promise((resolve, reject) => {
    const listener = createServer();
    listener.on("error", reject);
    listener.listen(0, HOST, () => {
      const { port } = listener.address();
      listener.close(() => resolve(port));
    });
  });
}

// Resolves true when a TCP connection to `port` opens, false when it is
// refused.
function accepts(port) {
  return new Promise((resolve, reject) => {
    const socket = connect(port, HOST);
    socket.on("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.on("error", (error) => error.code === "ECONNREFUSED" ? resolve(false) : reject(error));
  });
}

function hex(bytes) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

const ports = {};
for (const name of ["http", "https", "rtsp", "video", "control", "audio"]) ports[name] = await freePort();
const certificateDirectory = mkdtempSync(join(tmpdir(), "sandom-gamestream-"));

const events = [];
let pinRequired;
const pinRequiredEvent = new Promise((resolve) => pinRequired = resolve);
const server = startGameStreamServer({
  hostName: HOST_NAME,
  certificateDirectory,
  applicationTitle: "Sandom",
  bindAddress: HOST,
  ports,
}, (event) => {
  events.push(event);
  if (event.kind === "pinRequired") pinRequired(event);
});

try {
  const serverInfo = await (await fetch(`http://${HOST}:${ports.http}/serverinfo?uniqueid=${CLIENT}`)).text();
  for (const expected of [
    `<hostname>${HOST_NAME}</hostname>`,
    "<ServerCodecModeSupport>1</ServerCodecModeSupport>",
    `<HttpsPort>${ports.https}</HttpsPort>`,
    "<PairStatus>0</PairStatus>",
  ]) {
    if (!serverInfo.includes(expected)) throw new Error(`/serverinfo lacks ${expected}: ${serverInfo}`);
  }

  const salt = hex(crypto.getRandomValues(new Uint8Array(16)));
  const clientCertificate = hex(new TextEncoder().encode("-----BEGIN CERTIFICATE-----\nproof\n-----END CERTIFICATE-----\n"));
  const pairing = fetch(`http://${HOST}:${ports.http}/pair?uniqueid=${CLIENT}&phrase=getservercert&salt=${salt}` +
    `&clientcert=${clientCertificate}`);
  const event = await pinRequiredEvent;
  if (event.client !== CLIENT) throw new Error(`pinRequired names client ${event.client}; the request named ${CLIENT}`);

  // Stopping ends the pending request: Deno receives the pairing handler's
  // answer, while Node sees the connection closed unanswered and Bun
  // retries the GET and finds the port refused. Each runtime does the same
  // on every run.
  await server.stop();
  let pairingEnd;
  try {
    const answer = await pairing;
    const answerText = await answer.text();
    if (!answerText.includes("Server is shutting down.")) {
      throw new Error(`the pending pairing request received ${answer.status}: ${answerText}`);
    }
    pairingEnd = `answered ${answer.status}`;
  } catch (error) {
    if (error.message.startsWith("the pending pairing request received")) throw error;
    pairingEnd = `closed: ${error.cause?.message ?? error.code ?? error.message}`;
  }
  const open = [];
  for (const name of ["http", "https", "rtsp"]) if (await accepts(ports[name])) open.push(`${name} ${ports[name]}`);
  if (open.length > 0) throw new Error(`the stopped server still accepts connections on ${open.join(", ")}`);

  console.log(JSON.stringify({ runtime: gameStreamRuntime, ports, events, pairingEnd }));
} finally {
  rmSync(certificateDirectory, { recursive: true, force: true });
}
