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

## Client

`client/` is a GameStream client for these servers
(`sandom-gamestream-client`). `ClientIdentity::load_or_create(directory)`
keeps the client's certificate and ID; `pair(&identity, address, http_port,
pin)` pairs once, while the user enters the PIN on the device;
`Session::start(&identity, &paired, SessionConfig, events)` launches the
server's application and delivers each H.264 access unit as a
`SessionEvent::Frame`, from the session's own threads. `send_input` sends
pointer, keyboard, touch and text input, and `end` ends the session on the
server (`/cancel`). The client asks for encrypted video and gets it when the
server supports it (`Session::video_encrypted`).

On macOS, `VideoOutput::Metal { device }` decodes each frame with
VideoToolbox into a BGRA Metal texture of the host's `MTLDevice`
(`SessionEvent::Picture`, `Session::latest_picture`). The client's C
interface (`client/src/native.rs`, `libsandom_gamestream_client`) gives a
host plugin the identity, pairing, a decoding session with a doorbell, the
latest picture's texture, typed input calls and the end of the session.
`SessionConfig::packet_size` defaults to 1024, which fits the IPv6 minimum
MTU; 1392 fits a 1500-byte LAN. `cargo run --release -p
sandom-gamestream-client --example latency` measures the time from a send
to a decoded texture.

The server and the client share `protocol/` (`sandom-gamestream-protocol`):
the ciphers, the pairing identities and signatures, the control messages,
the input events, the packetizer and its counterpart, the frame assembler,
which recovers lost shards from the Reed-Solomon parity.
`client/tests/loopback.rs` pairs, streams and sends input against a server
in the same process.

## Unity

`unity/` is a Unity 6000.3.26f1 project (macOS, Metal, Gamma colour space)
whose components show GameStream servers on meshes in a scene
(`unity/Assets/GameStream/`):

- `GameStreamScreen` connects to one server and shows its video as the
  mesh's texture: a Metal texture of Unity's own device, with no copy. The
  first connection pairs and shows the PIN to enter on the device; the
  pairing is saved under `Application.persistentDataPath`, so later
  connections start the session directly. `Disconnect` ends the session on
  the server.
- `GameStreamScreenInput` sends the mouse, the wheel and the keyboard. A ray
  from the camera through the pointer hits the screen's `MeshCollider`, and
  the hit's texture coordinate is the point on the device's screen. A press
  on a screen gives it the keyboard.
- `GameStreamLatencyEcho` answers the latency tool: after a frame that
  showed a new picture, it sends that picture's frame index back.

`deno task unity:plugin` builds `libsandom_gamestream_client.dylib` and
copies it into `unity/Assets/Plugins/macOS/`; restart the Unity Editor after
it replaces the library. `deno task unity:players` also builds the scenes
(`GameStream → Rebuild Scenes` in the Editor) and two players:

- `unity/Build/TwoScreens.app`: two screens, the server on the GameStream
  ports of 127.0.0.1 on the left and the one at port offset 1000 on the
  right, for example `sandom-stream --serve` and `sandom-stream --serve
  --port-offset 1000` from Sandom's native host. With `--check DIRECTORY
  --pin PIN` it pairs with that PIN, types a word into each screen's page
  through the Input System, and captures the view before and after.
- `unity/Build/Latency.app --server ADDRESS --port-offset N --pin PIN`: one
  screen for `latency serve`, which then reports the time from each send to
  the camera frame that showed it.

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
