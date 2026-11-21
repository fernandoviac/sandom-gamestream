//! The control stream of a session, from Moonshine's
//! `src/session/stream/control/mod.rs`.
//!
//! Sandom's changes: decoded input goes to the server's event callback as
//! `ServerEvent::Input`; an IDR or reference-invalidation request becomes
//! `ServerEvent::KeyFrameRequested`. The HDR messages, the feedback channel
//! for gamepad rumble and LEDs, and the compositor input are removed.

use std::net::SocketAddr;
use std::time::Duration;

use async_shutdown::ShutdownManager;
use serde::{Deserialize, Serialize};
use tokio_enet::{Event, Host, HostConfig, Packet, PacketMode, PeerState};

use crate::crypto::{decrypt, encrypt};
use crate::session::{SessionKeysReceiver, SessionShutdownReason};
use crate::{EventSink, ServerEvent};

pub mod input;

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

const ENCRYPTION_TAG_LENGTH: usize = 16;
// Sequence number + tag + control message id.
const MINIMUM_ENCRYPTED_LENGTH: usize = 4 + ENCRYPTION_TAG_LENGTH + 4;

#[repr(u16)]
enum ControlMessageType {
	Encrypted = 0x0001,
	TerminationExtended = 0x0109,
	RumbleData = 0x010b,
	HdrMode = 0x010e,
	Ping = 0x0200,
	LossStats = 0x0201,
	FrameStats = 0x0204,
	InputData = 0x0206,
	RequestIdrFrame = 0x0302,
	InvalidateReferenceFrames = 0x0301,
	StartB = 0x0307,
	RumbleTriggers = 0x5500,
	SetMotionEvent = 0x5501,
	SetRgbLed = 0x5502,
	SetTriggerEffect = 0x5503,
}

impl TryFrom<u16> for ControlMessageType {
	type Error = ();

	fn try_from(v: u16) -> Result<Self, Self::Error> {
		match v {
			x if x == Self::Encrypted as u16 => Ok(Self::Encrypted),
			x if x == Self::TerminationExtended as u16 => Ok(Self::TerminationExtended),
			x if x == Self::RumbleData as u16 => Ok(Self::RumbleData),
			x if x == Self::HdrMode as u16 => Ok(Self::HdrMode),
			x if x == Self::Ping as u16 => Ok(Self::Ping),
			x if x == Self::LossStats as u16 => Ok(Self::LossStats),
			x if x == Self::FrameStats as u16 => Ok(Self::FrameStats),
			x if x == Self::InputData as u16 => Ok(Self::InputData),
			x if x == Self::RequestIdrFrame as u16 => Ok(Self::RequestIdrFrame),
			x if x == Self::InvalidateReferenceFrames as u16 => Ok(Self::InvalidateReferenceFrames),
			x if x == Self::StartB as u16 => Ok(Self::StartB),
			x if x == Self::RumbleTriggers as u16 => Ok(Self::RumbleTriggers),
			x if x == Self::SetMotionEvent as u16 => Ok(Self::SetMotionEvent),
			x if x == Self::SetRgbLed as u16 => Ok(Self::SetRgbLed),
			x if x == Self::SetTriggerEffect as u16 => Ok(Self::SetTriggerEffect),
			_ => Err(()),
		}
	}
}

#[derive(Debug)]
enum ControlMessage<'a> {
	Encrypted(EncryptedControlMessage),
	TerminationExtended,
	RumbleData,
	HdrMode,
	Ping,
	LossStats,
	FrameStats,
	InputData(&'a [u8]),
	RequestIdrFrame,
	// Without reference frame invalidation in the encoder, the range is unused.
	InvalidateReferenceFrames {
		#[allow(dead_code)]
		first: u32,
		#[allow(dead_code)]
		last: u32,
	},
	StartB,
	RumbleTriggers,
	SetMotionEvent,
	SetRgbLed,
	SetTriggerEffect,
}

impl<'a> ControlMessage<'a> {
	fn from_bytes(buffer: &'a [u8]) -> Result<Self, ()> {
		if buffer.len() < 4 {
			tracing::warn!(
				"Expected control message to have at least 4 bytes, got {}",
				buffer.len()
			);
			return Err(());
		}

		let length = u16::from_le_bytes(buffer[2..4].try_into().unwrap());
		if length as usize != buffer.len() - 4 {
			tracing::warn!(
				"Received incorrect packet length: expecting {length} bytes, but buffer says it should be {} bytes.",
				buffer.len() - 4
			);
			return Err(());
		}

		match u16::from_le_bytes(buffer[..2].try_into().unwrap()).try_into()? {
			ControlMessageType::Encrypted => {
				if buffer.len() < MINIMUM_ENCRYPTED_LENGTH {
					tracing::warn!(
						"Expected encrypted control message of at least {MINIMUM_ENCRYPTED_LENGTH} bytes, got buffer of {} bytes.",
						buffer.len()
					);
					return Err(());
				}

				let length = u16::from_le_bytes(buffer[2..4].try_into().unwrap());
				if (length as usize) < MINIMUM_ENCRYPTED_LENGTH {
					tracing::warn!(
						"Expected encrypted control message of at least {MINIMUM_ENCRYPTED_LENGTH} bytes, got reported length of {length} bytes."
					);
					return Err(());
				}

				let sequence_number = u32::from_le_bytes(buffer[4..8].try_into().unwrap());
				Ok(Self::Encrypted(EncryptedControlMessage {
					length,
					sequence_number,
					tag: buffer[8..8 + ENCRYPTION_TAG_LENGTH]
						.try_into()
						.map_err(|e| tracing::warn!("Failed to get tag from encrypted control message: {e}"))?,
					payload: buffer[8 + ENCRYPTION_TAG_LENGTH..].to_vec(),
				}))
			},
			ControlMessageType::Ping => Ok(Self::Ping),
			ControlMessageType::TerminationExtended => Ok(Self::TerminationExtended),
			ControlMessageType::RumbleData => Ok(Self::RumbleData),
			ControlMessageType::LossStats => Ok(Self::LossStats),
			ControlMessageType::FrameStats => Ok(Self::FrameStats),
			ControlMessageType::InputData => {
				// Length of the input event, excluding the length itself.
				let length = u32::from_be_bytes(buffer[4..8].try_into().unwrap());
				if length as usize != buffer.len() - 8 {
					tracing::warn!(
						"Failed to interpret input event message: expected {length} bytes, but buffer has {} bytes left.",
						buffer.len() - 8
					);
					return Err(());
				}

				Ok(Self::InputData(&buffer[8..]))
			},
			ControlMessageType::InvalidateReferenceFrames => {
				// Body (after the 4-byte type/length header), little-endian:
				//   firstFrameIndex(4), reserved1(4), lastFrameIndex(4), reserved2[3](12)
				// The client asks the host to stop referencing frames in the
				// range [first, last]. If the body is malformed, invalidate from
				// frame 0 so the encoder safely falls back to a full IDR.
				let (first, last) = if buffer.len() >= 16 {
					(
						u32::from_le_bytes(buffer[4..8].try_into().unwrap()),
						u32::from_le_bytes(buffer[12..16].try_into().unwrap()),
					)
				} else {
					(0, u32::MAX)
				};
				Ok(Self::InvalidateReferenceFrames { first, last })
			},
			ControlMessageType::RequestIdrFrame => Ok(Self::RequestIdrFrame),
			ControlMessageType::StartB => Ok(Self::StartB),
			ControlMessageType::HdrMode => Ok(Self::HdrMode),
			ControlMessageType::RumbleTriggers => Ok(Self::RumbleTriggers),
			ControlMessageType::SetMotionEvent => Ok(Self::SetMotionEvent),
			ControlMessageType::SetRgbLed => Ok(Self::SetRgbLed),
			ControlMessageType::SetTriggerEffect => Ok(Self::SetTriggerEffect),
		}
	}
}

#[derive(Debug)]
struct EncryptedControlMessage {
	length: u16,
	sequence_number: u32,
	tag: [u8; 16],
	payload: Vec<u8>,
}

impl EncryptedControlMessage {
	fn as_bytes(&self) -> Vec<u8> {
		let mut buffer = Vec::with_capacity(self.length as usize);

		buffer.extend((ControlMessageType::Encrypted as u16).to_le_bytes());
		buffer.extend(self.length.to_le_bytes());
		buffer.extend(self.sequence_number.to_le_bytes());
		buffer.extend(self.tag);
		buffer.extend(&self.payload);

		buffer
	}
}

fn encode_control(key: &[u8], sequence_number: u32, payload: &[u8]) -> Result<Vec<u8>, ()> {
	let mut initialization_vector = [0u8; 12];
	initialization_vector[0..4].copy_from_slice(&sequence_number.to_le_bytes());
	initialization_vector[10] = b'H';
	initialization_vector[11] = b'C';

	if key.len() != 16 {
		tracing::warn!("Key length has {} bytes, but expected {} bytes.", key.len(), 16);
		return Err(());
	}

	let mut tag = [0u8; 16];
	let payload = encrypt(payload, key, &initialization_vector, &mut tag)
		.map_err(|e| tracing::warn!("Failed to encrypt control data: {e}"))?;

	if payload.is_empty() {
		tracing::warn!("Failed to encrypt control data.");
		return Err(());
	}

	let message = EncryptedControlMessage {
		length: std::mem::size_of::<u32>() as u16 // Sequence number.
			 + ENCRYPTION_TAG_LENGTH as u16   // Tag.
			 + payload.len() as u16, // Payload.
		sequence_number,
		tag,
		payload,
	};

	Ok(message.as_bytes())
}


/// Build the payload for a termination extended control message.
///
/// The encrypted protocol uses V2 framing: `type(u16 LE) + length(u16 LE) + data`.
/// The client strips the length field after decryption (V2 → V1 conversion) and
/// then reads a 4-byte big-endian error code. Known NVST_DISCONN values are mapped
/// to client-side error constants (e.g. `0x80030023` → graceful termination).
fn build_termination_payload(error_code: u32) -> Vec<u8> {
	let payload_len = 4u16; // 4 bytes for error code.
	let mut buf = Vec::with_capacity(4 + payload_len as usize);
	buf.extend((ControlMessageType::TerminationExtended as u16).to_le_bytes());
	buf.extend(payload_len.to_le_bytes());
	buf.extend(error_code.to_be_bytes());
	buf
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
	if let Ok(packet) = encode_control(key, sequence_number, payload)
		&& let Some(peer) = host.peer_mut(peer_id)
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
				let mut control_message = match ControlMessage::from_bytes(packet.data()) {
					Ok(control_message) => control_message,
					Err(()) => break,
				};
				tracing::trace!("Received control message: {control_message:?}");

				// First check for encrypted control messages and decrypt them.
				let decrypted;
				if let ControlMessage::Encrypted(message) = control_message {
					let mut initialization_vector = [0u8; 12];
					initialization_vector[0..4].copy_from_slice(&message.sequence_number.to_le_bytes());
					initialization_vector[10] = b'C';
					initialization_vector[11] = b'C';

					let keys = &*keys_rx.borrow();
					let decrypted_result = decrypt(
						&message.payload,
						&keys.remote_input_key,
						&initialization_vector,
						&message.tag,
					);

					decrypted = match decrypted_result {
						Ok(decrypted) => decrypted,
						Err(e) => {
							tracing::warn!("Failed to decrypt control message: {:?}", e);
							continue;
						},
					};

					control_message = match ControlMessage::from_bytes(&decrypted) {
						Ok(decrypted_message) => decrypted_message,
						Err(()) => continue,
					};

					tracing::trace!("Decrypted control message: {control_message:?}");
				}

				match control_message {
					ControlMessage::Encrypted(_) => {
						unreachable!("Encrypted control messages should be decrypted already.")
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
					ControlMessage::InputData(event) => {
						if let Ok(event) = input::InputEvent::from_bytes(event) {
							events(ServerEvent::Input(event));
						}
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
	// NVST_DISCONN_SERVER_TERMINATED_CLOSED (0x80030023) is recognized by the
	// client as a graceful shutdown so it does not display an error.
	let termination_payload = build_termination_payload(0x80030023);
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
