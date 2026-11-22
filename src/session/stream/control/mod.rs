//! The control stream of a session, from Moonshine's
//! `src/session/stream/control/mod.rs`.
//!
//! Sandom's changes: decoded input goes to the server's event callback as
//! `ServerEvent::Input`; an IDR or reference-invalidation request becomes
//! `ServerEvent::KeyFrameRequested`. The HDR messages, the feedback channel
//! for gamepad rumble and LEDs, and the compositor input are removed. The
//! message formats are in `sandom_gamestream_protocol::control`, which the
//! client shares.

use std::net::SocketAddr;
use std::time::Duration;

use async_shutdown::ShutdownManager;
use sandom_gamestream_protocol::control::{self, ControlMessage, Direction};
use sandom_gamestream_protocol::input::InputEvent;
use serde::{Deserialize, Serialize};
use tokio_enet::{Event, Host, HostConfig, Packet, PacketMode, PeerState};

use crate::session::{SessionKeysReceiver, SessionShutdownReason};
use crate::{EventSink, ServerEvent};

/// Configuration for the control stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ControlStreamConfig {
	/// Port to use for streaming control data.
	pub port: u16,
}

impl Default for ControlStreamConfig {
	fn default() -> Self {
		Self { port: 47999 }
	}
}

/// Send an encrypted control packet to the connected peer if it exists and is connected.
fn send_to_peer(
	host: &mut Host,
	peer_id: tokio_enet::PeerId,
	key: &[u8],
	sequence_number: u32,
	payload: &[u8],
	label: &str,
) {
	let packet = match control::encrypt(key, sequence_number, Direction::HostToClient, payload) {
		Ok(packet) => packet,
		Err(error) => {
			tracing::warn!("Failed to encrypt the {label} message: {error:#}");
			return;
		},
	};
	if let Some(peer) = host.peer_mut(peer_id)
		&& peer.state() == PeerState::Connected
	{
		let _ = peer
			.send(0, Packet::new(packet.as_slice(), PacketMode::ReliableSequenced))
			.map_err(|e| tracing::warn!("Failed to send {label} to peer: {e}"));
	}
}

pub(crate) struct ControlStream {
	stop: ShutdownManager<SessionShutdownReason>,
	host: Host,
}

impl ControlStream {
	pub fn new(
		config: &ControlStreamConfig,
		address: &str,
		stop_session_manager: ShutdownManager<SessionShutdownReason>,
	) -> Result<Self, ()> {
		let socket_address = SocketAddr::new(
			address
				.parse()
				.map_err(|e| tracing::warn!("Failed to parse address ({}): {e}", address))?,
			config.port,
		);

		let host_config = HostConfig {
			address: Some(socket_address),
			peer_count: 128,
			channel_limit: 0x30,
			..Default::default()
		};

		let host = Host::new(host_config).map_err(|e| tracing::error!("Failed to create enet host: {e}"))?;

		tracing::debug!("Listening for control messages on {:?}", socket_address);

		Ok(Self {
			stop: stop_session_manager,
			host,
		})
	}

	pub fn start(self, stream_timeout: u64, keys_rx: SessionKeysReceiver, events: EventSink) {
		let Self {
			stop: stop_session_manager,
			host,
		} = self;

		tokio::spawn(async move {
			run_control_loop(stream_timeout, host, keys_rx, events, stop_session_manager).await;
		});
	}
}

async fn run_control_loop(
	stream_timeout: u64,
	mut host: Host,
	keys_rx: SessionKeysReceiver,
	events: EventSink,
	stop_session_manager: ShutdownManager<SessionShutdownReason>,
) {
	// Trigger session shutdown when the control stream stops.
	let _session_stop_token = stop_session_manager.trigger_shutdown_token(SessionShutdownReason::ControlStreamStopped);
	let _delay_stop = stop_session_manager.delay_shutdown_token();

	let mut stop_deadline = std::time::Instant::now() + std::time::Duration::from_secs(stream_timeout);

	// Sequence number of messages to the client.
	let sequence_number = 0u32;
	// Track which peer slot the client is connected to.
	let mut connected_peer: Option<tokio_enet::PeerId> = None;

	while !stop_session_manager.is_shutdown_triggered() {
		// Check if the timeout has passed.
		if std::time::Instant::now() > stop_deadline {
			tracing::info!(
				"Stopping because we haven't received a ping for {} seconds.",
				stream_timeout
			);
			break;
		}

		match host
			.service(Duration::from_millis(10))
			.await
			.map_err(|e| tracing::error!("Failure in enet host: {e}"))
		{
			Ok(Some(Event::Connect { peer_id, .. })) => {
				connected_peer = Some(peer_id);
			},
			Ok(Some(Event::Disconnect { peer_id, .. })) => {
				// A client that disconnects may resume; the ping timeout ends the session.
				if connected_peer == Some(peer_id) {
					connected_peer = None;
				}
			},
			Ok(Some(Event::Receive { ref packet, .. })) => {
				let control_message = match ControlMessage::from_bytes(packet.data()) {
					Ok(control_message) => control_message,
					Err(error) => {
						tracing::warn!("Stopping the control stream at an unreadable message: {error:#}");
						break;
					},
				};
				tracing::trace!("Received control message: {control_message:?}");

				// An encrypted message carries the message to handle.
				let decrypted;
				let control_message = match control_message {
					ControlMessage::Encrypted { sequence_number, tag, ciphertext } => {
						let key = keys_rx.borrow().remote_input_key.clone();
						decrypted =
							match control::decrypt(&key, Direction::ClientToHost, sequence_number, &tag, ciphertext) {
								Ok(decrypted) => decrypted,
								Err(error) => {
									tracing::warn!("Failed to decrypt control message: {error:#}");
									continue;
								},
							};
						match ControlMessage::from_bytes(&decrypted) {
							Ok(message) => message,
							Err(error) => {
								tracing::warn!("Skipped an unreadable decrypted control message: {error:#}");
								continue;
							},
						}
					},
					message => message,
				};

				match control_message {
					ControlMessage::Encrypted { .. } => {
						tracing::warn!("Skipped an encrypted control message inside an encrypted message.");
					},
					// Without reference frame invalidation in the encoder, a lost
					// reference recovers through a key frame.
					ControlMessage::RequestIdrFrame | ControlMessage::InvalidateReferenceFrames { .. } => {
						events(ServerEvent::KeyFrameRequested);
					},
					ControlMessage::StartB => {
						// The video socket sends once the client PINGs it.
					},
					ControlMessage::Ping => {
						stop_deadline = std::time::Instant::now() + std::time::Duration::from_secs(stream_timeout);
					},
					ControlMessage::InputData(event) => match InputEvent::from_bytes(event) {
						Ok(Some(event)) => events(ServerEvent::Input(event)),
						Ok(None) => {},
						Err(error) => tracing::warn!("Skipped an input event: {error:#}"),
					},
					skipped_message => {
						tracing::trace!("Skipped control message: {skipped_message:?}");
					},
				};
			},
			Ok(None) => (),
			Err(_) => break,
		}
	}

	tracing::debug!("Control stream stopped.");

	// Notify the client of graceful termination before closing the connection.
	// The client recognizes TERMINATION_GRACEFUL as a graceful shutdown, so it
	// does not display an error.
	let termination_payload = control::termination_message(control::TERMINATION_GRACEFUL);
	if let Some(peer_id) = connected_peer {
		let key = keys_rx.borrow().remote_input_key.clone();
		send_to_peer(
			&mut host,
			peer_id,
			&key,
			sequence_number,
			&termination_payload,
			"termination",
		);
	}
	let _ = host.flush().await;

	// Drop the ENet host before the delay token, so the next session's control
	// stream can bind the same port.
	drop(host);
}
