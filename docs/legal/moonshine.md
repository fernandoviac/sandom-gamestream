# Moonshine protocol source

Upstream: <https://github.com/hgaiser/moonshine>.
Commit: `e6a7391d8d8245869dcd29d20a1fd5208b4f139c`.
Licence: BSD-2-Clause. Copyright (c) 2024, Hans Gaiser.

[LICENSE-MOONSHINE](../../LICENSE-MOONSHINE) contains the unchanged upstream licence.
The local source resides in `src/`.

## Extracted items

Line ranges refer to the upstream commit above.

| Source path | Lines | Items | Local path |
| --- | --- | --- | --- |
| `src/session/mod.rs` | 40–74 | `SessionKeyData`, `SessionKeysReceiver`, `SessionKeysSender`, `SessionKeys`, `SessionKeys::new`, `SessionKeys::clone_rx` | `src/session/mod.rs` |
| `src/session/stream/audio/mod.rs` | 22–285 | `AudioStreamConfig`, `AudioChannels` without its PulseAudio functions, `OpusStreamConfig`, the `OPUS_*` constants, `ALL_AUDIO_CONFIGS`, `AudioConfig`, `AudioStreamContext` | `src/session/stream/audio.rs` |
| `src/session/stream/video/mod.rs` | 25–152 | `VideoStreamConfig` and its default, `keyframe_burst_bytes` | `src/session/stream/video/mod.rs` |
| `src/session/stream/video/mod.rs` | 342–446 | `VideoFormat`, `VideoDynamicRange`, `VideoChromaSampling`, `VideoStreamContext` | `src/session/stream/video/mod.rs` |
| `src/session/stream/control/mod.rs` | 46–258, 395–428 | control message types, parsing, encryption, `build_termination_payload`, `send_to_peer` | `src/session/stream/control/mod.rs` |
| `src/session/stream/control/input/mod.rs` | 1–137, 622–639 | `InputEventType`, `InputEvent::from_bytes`, the text-event tests | `src/session/stream/control/input/mod.rs` |

The extracted modules contain the protocol types and their functions, without the application, compositor, audio capture, encoder, or gamepad code.

## Local modifications

- `src/tls.rs` keeps the certificate and private-key operations from upstream lines 372–417. `load_or_create_certificate` takes the certificate path and private-key path directly. It does not import Moonshine's `Config`.
- `src/state.rs`: `PersistentState::new` takes the state file path. The unused device-list and unpair functions are removed.
- `src/clients.rs`: `ClientManager::new` takes the state file path. The control-panel functions (pairing requests, device list, rename, unpair, PIN submission) are removed.
- `src/crypto.rs`: the audio CBC cipher is removed.
- `src/webserver/mod.rs`: one application with ID 1 replaces the application manager. `/appasset` answers not found. The MAC address lookup and the `/pin` page are removed. `ServerCodecModeSupport` advertises H.264 only, and `MaxLumaPixelsHEVC` is zero.
- `src/webserver/pairing.rs`: a pairing request emits `ServerEvent::PinRequired` instead of a desktop notification.
- `src/session/stream/control/mod.rs`: decoded input is emitted as `ServerEvent::Input`; IDR and reference-invalidation requests are emitted as `ServerEvent::KeyFrameRequested`. HDR messages, gamepad feedback and the compositor input are removed.
- `src/session/stream/control/input/`: the payload types are public. Gamepad events are logged and dropped. The input handler and `remap.rs` are removed.
- `src/session/stream/video/packetizer.rs`: the unused `FrameType::Recovery` is removed.
- `src/session/stream/mod.rs`: the start-signal module and the unused `RtpHeader` are removed.
- `src/rtsp.rs`: imports the local `SessionManager`.

## Local code

These parts are written for Sandom and are not upstream code:

- `src/lib.rs`: `Server`, `ServerConfig`, `ServerPorts`, `ServerEvent`, `Codec`, `ShutdownReason`.
- `src/session/mod.rs`: `SessionContext`, `SessionShutdownReason` and `SessionManager`, which replace Moonshine's session manager.
- `src/session/stream/video/mod.rs`: `VideoStream` and `VideoStreamHandle`, which packetize access units from the caller and send them to the client.
- `tests/server.rs`.

## Copied modules

The following source files retain their upstream paths, with the modifications above:

- `src/webserver/mod.rs`
- `src/webserver/pairing.rs`
- `src/tls.rs`
- `src/clients.rs`
- `src/rtsp.rs`
- `src/crypto.rs`
- `src/state.rs`
- `src/session/stream/mod.rs`
- `src/session/stream/control/input/keyboard.rs`
- `src/session/stream/control/input/mouse.rs`
- `src/session/stream/control/input/touch.rs`
- `src/session/stream/video/packetizer.rs`
- `src/session/stream/video/shard_batch.rs`
- `src/session/stream/video/gso_socket.rs`
