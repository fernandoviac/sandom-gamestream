//! The control stream's messages, from Moonshine's
//! `src/session/stream/control/mod.rs`.
//!
//! A message is `type (u16 LE) ‖ length (u16 LE) ‖ body`, where `length`
//! counts the body. An encrypted message has type `ENCRYPTED` and the body
//! `sequence number (u32 LE) ‖ tag (16) ‖ ciphertext`; its plaintext is a
//! message in the same framing. The initialization vector is the sequence
//! number (u32 LE), six zero bytes, and two bytes that name the direction:
//! `CC` from the client, `HC` from the host.

use anyhow::{Result, bail, ensure};

use crate::crypto::{TAG_LENGTH, gcm_decrypt, gcm_encrypt};

pub const ENCRYPTED: u16 = 0x0001;
pub const TERMINATION_EXTENDED: u16 = 0x0109;
pub const RUMBLE_DATA: u16 = 0x010b;
pub const HDR_MODE: u16 = 0x010e;
pub const PING: u16 = 0x0200;
pub const LOSS_STATS: u16 = 0x0201;
pub const FRAME_STATS: u16 = 0x0204;
pub const INPUT_DATA: u16 = 0x0206;
pub const INVALIDATE_REFERENCE_FRAMES: u16 = 0x0301;
pub const REQUEST_IDR_FRAME: u16 = 0x0302;
pub const START_B: u16 = 0x0307;
pub const RUMBLE_TRIGGERS: u16 = 0x5500;
pub const SET_MOTION_EVENT: u16 = 0x5501;
pub const SET_RGB_LED: u16 = 0x5502;
pub const SET_TRIGGER_EFFECT: u16 = 0x5503;

/// Sequence number, tag and the smallest inner message (type and length).
const MINIMUM_ENCRYPTED_BODY: usize = 4 + TAG_LENGTH + 4;

/// The error code of a server's graceful end of a session
/// (NVST_DISCONN_SERVER_TERMINATED_CLOSED).
pub const TERMINATION_GRACEFUL: u32 = 0x8003_0023;

/// The sender of an encrypted message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
	ClientToHost,
	HostToClient,
}

/// One control message.
#[derive(Debug)]
pub enum ControlMessage<'a> {
	Encrypted { sequence_number: u32, tag: [u8; TAG_LENGTH], ciphertext: &'a [u8] },
	TerminationExtended { error_code: u32 },
	InputData(&'a [u8]),
	InvalidateReferenceFrames { first: u32, last: u32 },
	RequestIdrFrame,
	StartB,
	Ping,
	/// A message that this protocol subset reads no field of.
	Other(u16),
}

impl<'a> ControlMessage<'a> {
	pub fn from_bytes(buffer: &'a [u8]) -> Result<Self> {
		ensure!(buffer.len() >= 4, "a control message has at least 4 bytes, got {}", buffer.len());
		let message_type = u16::from_le_bytes([buffer[0], buffer[1]]);
		let length = u16::from_le_bytes([buffer[2], buffer[3]]) as usize;
		let body = &buffer[4..];
		ensure!(
			length == body.len(),
			"control message 0x{message_type:04x} states a {length}-byte body and has {} bytes",
			body.len()
		);
		Ok(match message_type {
			ENCRYPTED => {
				ensure!(
					body.len() >= MINIMUM_ENCRYPTED_BODY,
					"an encrypted control message has at least {MINIMUM_ENCRYPTED_BODY} bytes of body, got {}",
					body.len()
				);
				Self::Encrypted {
					sequence_number: u32::from_le_bytes(body[0..4].try_into().unwrap()),
					tag: body[4..4 + TAG_LENGTH].try_into().unwrap(),
					ciphertext: &body[4 + TAG_LENGTH..],
				}
			},
			TERMINATION_EXTENDED => {
				ensure!(body.len() >= 4, "a termination message has a 4-byte error code, got {} bytes", body.len());
				Self::TerminationExtended { error_code: u32::from_be_bytes(body[0..4].try_into().unwrap()) }
			},
			INPUT_DATA => {
				ensure!(body.len() >= 4, "an input message has a 4-byte length, got {} bytes", body.len());
				let event_length = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
				ensure!(
					event_length == body.len() - 4,
					"an input message states a {event_length}-byte event and has {} bytes",
					body.len() - 4
				);
				Self::InputData(&body[4..])
			},
			INVALIDATE_REFERENCE_FRAMES => {
				// first (u32 LE), reserved (4), last (u32 LE), reserved (12). A
				// malformed range invalidates from frame 0, which the encoder
				// answers with a key frame.
				if body.len() >= 12 {
					Self::InvalidateReferenceFrames {
						first: u32::from_le_bytes(body[0..4].try_into().unwrap()),
						last: u32::from_le_bytes(body[8..12].try_into().unwrap()),
					}
				} else {
					Self::InvalidateReferenceFrames { first: 0, last: u32::MAX }
				}
			},
			REQUEST_IDR_FRAME => Self::RequestIdrFrame,
			START_B => Self::StartB,
			PING => Self::Ping,
			RUMBLE_DATA | HDR_MODE | LOSS_STATS | FRAME_STATS | RUMBLE_TRIGGERS | SET_MOTION_EVENT | SET_RGB_LED
			| SET_TRIGGER_EFFECT => Self::Other(message_type),
			unknown => bail!("unknown control message type 0x{unknown:04x}"),
		})
	}
}

/// A message of `message_type` with `body`.
pub fn message(message_type: u16, body: &[u8]) -> Vec<u8> {
	let mut buffer = Vec::with_capacity(4 + body.len());
	buffer.extend(message_type.to_le_bytes());
	buffer.extend((body.len() as u16).to_le_bytes());
	buffer.extend(body);
	buffer
}

/// The input message that carries `event`, an input event's bytes.
pub fn input_message(event: &[u8]) -> Vec<u8> {
	let mut body = Vec::with_capacity(4 + event.len());
	body.extend((event.len() as u32).to_be_bytes());
	body.extend(event);
	message(INPUT_DATA, &body)
}

/// The termination message with `error_code`.
pub fn termination_message(error_code: u32) -> Vec<u8> {
	message(TERMINATION_EXTENDED, &error_code.to_be_bytes())
}

fn initialization_vector(sequence_number: u32, direction: Direction) -> [u8; 12] {
	let mut iv = [0u8; 12];
	iv[0..4].copy_from_slice(&sequence_number.to_le_bytes());
	iv[10..12].copy_from_slice(match direction {
		Direction::ClientToHost => b"CC",
		Direction::HostToClient => b"HC",
	});
	iv
}

/// The encrypted message that carries `plaintext`, a control message, from
/// `direction`'s sender.
pub fn encrypt(key: &[u8], sequence_number: u32, direction: Direction, plaintext: &[u8]) -> Result<Vec<u8>> {
	let (ciphertext, tag) = gcm_encrypt(plaintext, key, &initialization_vector(sequence_number, direction))?;
	let mut body = Vec::with_capacity(4 + TAG_LENGTH + ciphertext.len());
	body.extend(sequence_number.to_le_bytes());
	body.extend(tag);
	body.extend(ciphertext);
	Ok(message(ENCRYPTED, &body))
}

/// The plaintext of an encrypted message from `direction`'s sender.
pub fn decrypt(
	key: &[u8],
	direction: Direction,
	sequence_number: u32,
	tag: &[u8; TAG_LENGTH],
	ciphertext: &[u8],
) -> Result<Vec<u8>> {
	gcm_decrypt(ciphertext, key, &initialization_vector(sequence_number, direction), tag)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn an_encrypted_input_message_decrypts_in_its_direction_only() {
		let key = [5u8; 16];
		let frame = encrypt(&key, 9, Direction::ClientToHost, &input_message(b"event")).unwrap();
		let ControlMessage::Encrypted { sequence_number, tag, ciphertext } = ControlMessage::from_bytes(&frame).unwrap()
		else {
			panic!("not an encrypted message");
		};
		assert_eq!(sequence_number, 9);
		assert!(decrypt(&key, Direction::HostToClient, sequence_number, &tag, ciphertext).is_err());
		let plaintext = decrypt(&key, Direction::ClientToHost, sequence_number, &tag, ciphertext).unwrap();
		assert!(matches!(ControlMessage::from_bytes(&plaintext).unwrap(), ControlMessage::InputData(b"event")));
	}

	#[test]
	fn a_termination_message_carries_its_error_code() {
		let message = termination_message(TERMINATION_GRACEFUL);
		assert!(matches!(
			ControlMessage::from_bytes(&message).unwrap(),
			ControlMessage::TerminationExtended { error_code: TERMINATION_GRACEFUL }
		));
	}

	#[test]
	fn a_wrong_length_is_refused() {
		let mut message = message(PING, &[]);
		message.push(0);
		assert!(ControlMessage::from_bytes(&message).is_err());
	}
}
