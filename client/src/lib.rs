//! A GameStream client for Sandom GameStream servers.
//!
//! `ClientIdentity::load_or_create` keeps the client's certificate and ID;
//! `pair` pairs with a server once, with the PIN that the user enters on the
//! device; `Session::start` launches the server's application and streams
//! its video as H.264 access units through an event callback, and
//! `Session::send_input` sends pointer, keyboard, touch and text input. On
//! macOS, `decoder::Decoder` decodes the access units with VideoToolbox into
//! Metal textures. The wire formats are in `sandom_gamestream_protocol`,
//! which the server shares.

#[cfg(target_os = "macos")]
mod apple;
#[cfg(target_os = "macos")]
pub mod decoder;
pub mod h264;
mod http;
#[cfg(target_os = "macos")]
pub mod native;
mod pairing;
mod rtsp;
mod session;

pub use pairing::{ClientIdentity, PairedServer, pair};
pub use sandom_gamestream_protocol::input;
pub use sandom_gamestream_protocol::video::{Frame, FrameType};
pub use session::{Session, SessionConfig, SessionEvent, VideoOutput};
