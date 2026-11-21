// The GameStream server's binding of this runtime: Deno's FFI (deno.js), or
// the Node-API addon (node.js) under Node and Bun. Each exports
// `gameStreamLibrary()`, the library or addon of this platform's installed
// package, else the one built in this checkout, else null, and
// `startGameStreamServer(config, onEvent, path)`.
//
// `config`: `hostName`, the name Moonlight shows; `certificateDirectory`,
// where the server keeps its certificate and paired clients;
// `applicationTitle`, the one application Moonlight lists; `bindAddress`,
// such as "0.0.0.0"; and optional `ports` (`http`, `https`, `rtsp`,
// `video`, `control`, `audio`), the GameStream ports by default. The
// native library opens these ports itself: Deno's `--allow-net` does not
// govern them. `onEvent` receives each event record of src/native.rs.

const runtime = globalThis.Deno !== undefined ? "deno" : globalThis.Bun !== undefined ? "bun"
  : globalThis.process?.versions?.node !== undefined ? "node" : null;
if (runtime === null) {
  throw new Error("The GameStream server binds Deno, Bun and Node; this runtime is none of them");
}

const binding = runtime === "deno" ? await import("./deno.js") : await import("./node.js");

export const gameStreamRuntime = runtime;
export const { gameStreamLibrary, startGameStreamServer } = binding;
