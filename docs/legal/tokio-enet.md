# tokio-enet source

Upstream: <https://github.com/hgaiser/tokio-enet>, crate `tokio-enet` 0.1.1.
Commit: `a331fa1d0c1aa5f076ab52913dbf8be3c62777b1`.
Licence: BSD-2-Clause. Copyright (c) 2026, Hans Gaiser.

[vendor/tokio-enet/LICENSE](../../vendor/tokio-enet/LICENSE) contains the unchanged upstream licence.
The local source resides in `vendor/tokio-enet/`: the crate's published
files (`src/`, `tests/`, `Cargo.toml`, `README.md`, `CHANGELOG.md`). The
workspace's `[patch.crates-io]` builds the server and the client with it.

## Local modifications

- `src/host.rs`, `send_outgoing_commands`: the retransmission timeout of a
  reliable command is the round trip and four of its variances, as in C
  ENet's `enet_protocol_send_reliable_outgoing_commands`. Upstream also
  takes at least the peer's timeout minimum (5000 ms), which holds a lost
  reliable command, and every command after it on a sequenced channel, for
  at least 5 seconds.

## Build

Cargo builds the crate from `vendor/tokio-enet/` as part of the workspace.
Its own tests run outside the workspace: copy the directory elsewhere and
run `cargo test` there.
