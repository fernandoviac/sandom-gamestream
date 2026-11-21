# Sandom GameStream

A GameStream server for Moonlight clients. It runs pairing, the HTTP and HTTPS
endpoints, RTSP, and the encrypted video and control streams, and it reports
what happens as events: a pairing PIN to enter, a session with the client's
resolution, frame rate and bit rate, a request for a key frame, the client's
input, and the end of a session. The caller encodes the video and sends each
H.264 access unit; the server packetizes and sends it.

The protocol modules come from Moonshine (BSD 2-Clause), without its
compositor, encoder, audio server, application launcher and control panel.
`docs/legal/moonshine.md` records the upstream commit, what was extracted and
what changed.

## Use

From Rust, `Server::start(ServerConfig, events)` starts the server on its
own runtime threads and calls `events` with each `ServerEvent`.
`Server::send_access_unit(data, key, timestamp_us)` sends one encoded access
unit of the current session; `end_session` ends the session and `stop` stops
the server. Sandom's Rust host streams a document with it
(`sandom-stream --serve`).

From Deno, Bun and Node, `bindings/mod.js` gives
`startGameStreamServer(config, onEvent)`: Deno loads the library's C
interface (`src/native.rs`) through its FFI, Node and Bun load the Node-API
addon of `node/`. The server's events arrive as records (`pinRequired`,
`sessionStarted`, `keyFrameRequested`, `input`, `sessionEnded`), and the
server has `sendAccessUnit`, `endSession` and `stop`. The native library
opens its own ports, which Deno's `--allow-net` does not govern.

Sandom's `src/game-stream.js` streams a document over it, encoding with the
native GPU host (VideoToolbox on macOS, NVENC on Linux).

The server advertises H.264 only and no audio. Moonlight finds it through the
system's mDNS responder where one publishes it, or by its address.

## Commands

- `deno task build` builds the library and the addon.
- `cargo test --release` runs `tests/server.rs`.
- `deno task proof` runs `tools/binding-proof.js`: the server under the
  runtime's binding, `/serverinfo`, the first phase of pairing, and a stop
  that closes every port. Run it under Bun and Node as `bun` or
  `node tools/binding-proof.js`.

## Packages

`@sandom/gamestream` holds the bindings; one platform package for each of
macOS arm64 and x64 and Linux x64 and arm64 holds the library, the addon and
the third-party notices of the crates they contain. `tools/build-macos.js`
and `tools/build-linux.js` build the libraries, `tools/stage-packages.js`
stages the packages, `tools/install-proof.js` (or
`tools/linux-install-proof.js` in a Debian bookworm container) installs the
packed archives offline and starts the server under Deno, Bun and Node, and
`deno task npm:prepare` packs every package from `HEAD` into
`.publication/npm` for review. Nothing here publishes.

## Licence

The code written for Sandom is Apache-2.0 (`LICENSE`); the Moonshine modules
are BSD 2-Clause (`LICENSE-MOONSHINE`). See `NOTICE`.
