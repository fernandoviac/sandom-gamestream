//! Input events of the control stream, from Moonshine's
//! `src/session/stream/control/input/mod.rs`.
//!
//! An event is its type (u32 LE) and its payload. Sandom's changes: each
//! event writes its bytes as well as reads them, for the client. Gamepad
//! events are not supported: reading one is an error that names it.
//! Moonshine's input handler, which forwarded the events to its compositor
//! and to virtual gamepads, is removed.

use anyhow::{Result, bail, ensure};
use strum_macros::FromRepr;

pub use self::keyboard::Key;
pub use self::mouse::{MouseButton, MouseMoveAbsolute, MouseMoveRelative, MouseScrollHorizontal, MouseScrollVertical};
pub use self::touch::{Pen, PenToolKind, PointerEventKind, Touch};

mod keyboard;
mod mouse;
mod touch;

#[derive(FromRepr)]
#[repr(u32)]
enum InputEventType {
	KeyDown = 0x00000003,
	KeyUp = 0x00000004,
	MouseMoveAbsolute = 0x00000005,
	MouseMoveRelative = 0x00000007,
	MouseButtonDown = 0x00000008,
	MouseButtonUp = 0x00000009,
	MouseScrollVertical = 0x0000000A,
	MouseScrollHorizontal = 0x55000001,
	Touch = 0x55000002,
	Pen = 0x55000003,
	GamepadInfo = 0x55000004, // Called ControllerArrival in Moonlight.
	GamepadTouch = 0x55000005,
	GamepadMotion = 0x55000006,
	GamepadBattery = 0x55000007,
	GamepadUpdate = 0x0000000C,
	GamepadEnableHaptics = 0x0000000D,
	Utf8Text = 0x00000017,
}

/// One input event of a client.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
	KeyDown(Key),
	KeyUp(Key),
	MouseMoveAbsolute(MouseMoveAbsolute),
	MouseMoveRelative(MouseMoveRelative),
	MouseButtonDown(MouseButton),
	MouseButtonUp(MouseButton),
	MouseScrollVertical(MouseScrollVertical),
	MouseScrollHorizontal(MouseScrollHorizontal),
	Touch(Touch),
	Pen(Pen),
	Utf8Text(String),
}

impl InputEvent {
	/// The event of `buffer`, or None for a key event without a key (see
	/// `Key::from_bytes`).
	pub fn from_bytes(buffer: &[u8]) -> Result<Option<Self>> {
		ensure!(buffer.len() >= 4, "an input event has a 4-byte type, got {} bytes", buffer.len());
		let event_type = u32::from_le_bytes(buffer[..4].try_into().unwrap());
		let payload = &buffer[4..];
		Ok(Some(match InputEventType::from_repr(event_type) {
			Some(InputEventType::KeyDown) => match Key::from_bytes(payload)? {
				Some(key) => InputEvent::KeyDown(key),
				None => return Ok(None),
			},
			Some(InputEventType::KeyUp) => match Key::from_bytes(payload)? {
				Some(key) => InputEvent::KeyUp(key),
				None => return Ok(None),
			},
			Some(InputEventType::MouseMoveAbsolute) => InputEvent::MouseMoveAbsolute(MouseMoveAbsolute::from_bytes(payload)?),
			Some(InputEventType::MouseMoveRelative) => InputEvent::MouseMoveRelative(MouseMoveRelative::from_bytes(payload)?),
			Some(InputEventType::MouseButtonDown) => InputEvent::MouseButtonDown(MouseButton::from_bytes(payload)?),
			Some(InputEventType::MouseButtonUp) => InputEvent::MouseButtonUp(MouseButton::from_bytes(payload)?),
			Some(InputEventType::MouseScrollVertical) => {
				InputEvent::MouseScrollVertical(MouseScrollVertical::from_bytes(payload)?)
			},
			Some(InputEventType::MouseScrollHorizontal) => {
				InputEvent::MouseScrollHorizontal(MouseScrollHorizontal::from_bytes(payload)?)
			},
			Some(InputEventType::Touch) => InputEvent::Touch(Touch::from_bytes(payload)?),
			Some(InputEventType::Pen) => InputEvent::Pen(Pen::from_bytes(payload)?),
			Some(
				InputEventType::GamepadInfo
				| InputEventType::GamepadTouch
				| InputEventType::GamepadMotion
				| InputEventType::GamepadBattery
				| InputEventType::GamepadUpdate
				| InputEventType::GamepadEnableHaptics,
			) => bail!("gamepad event 0x{event_type:08x}: this server supports no gamepads"),
			Some(InputEventType::Utf8Text) => {
				let text = String::from_utf8(payload.to_vec())
					.map_err(|error| anyhow::anyhow!("a text event is not UTF-8: {error}"))?;
				ensure!(!text.is_empty(), "a text event has no text");
				InputEvent::Utf8Text(text)
			},
			None => bail!("unknown input event type 0x{event_type:08x}"),
		}))
	}

	/// The bytes of this event: its type and payload.
	pub fn to_bytes(&self) -> Vec<u8> {
		let (event_type, payload): (InputEventType, Vec<u8>) = match self {
			InputEvent::KeyDown(key) => (InputEventType::KeyDown, key.to_bytes().to_vec()),
			InputEvent::KeyUp(key) => (InputEventType::KeyUp, key.to_bytes().to_vec()),
			InputEvent::MouseMoveAbsolute(event) => (InputEventType::MouseMoveAbsolute, event.to_bytes().to_vec()),
			InputEvent::MouseMoveRelative(event) => (InputEventType::MouseMoveRelative, event.to_bytes().to_vec()),
			InputEvent::MouseButtonDown(button) => (InputEventType::MouseButtonDown, button.to_bytes().to_vec()),
			InputEvent::MouseButtonUp(button) => (InputEventType::MouseButtonUp, button.to_bytes().to_vec()),
			InputEvent::MouseScrollVertical(event) => (InputEventType::MouseScrollVertical, event.to_bytes().to_vec()),
			InputEvent::MouseScrollHorizontal(event) => {
				(InputEventType::MouseScrollHorizontal, event.to_bytes().to_vec())
			},
			InputEvent::Touch(touch) => (InputEventType::Touch, touch.to_bytes().to_vec()),
			InputEvent::Pen(pen) => (InputEventType::Pen, pen.to_bytes().to_vec()),
			InputEvent::Utf8Text(text) => (InputEventType::Utf8Text, text.as_bytes().to_vec()),
		};
		let mut buffer = (event_type as u32).to_le_bytes().to_vec();
		buffer.extend(payload);
		buffer
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_utf8_text_event() {
		let payload = [0x17, 0x00, 0x00, 0x00, b'h', b'e', b'y'];
		assert_eq!(InputEvent::from_bytes(&payload).unwrap(), Some(InputEvent::Utf8Text("hey".to_string())));
	}

	#[test]
	fn rejects_empty_utf8_text_event() {
		let payload = [0x17, 0x00, 0x00, 0x00];
		assert!(InputEvent::from_bytes(&payload).is_err());
	}

	#[test]
	fn every_event_round_trips() {
		for event in [
			InputEvent::KeyDown(Key::B),
			InputEvent::KeyUp(Key::Escape),
			InputEvent::MouseMoveAbsolute(MouseMoveAbsolute { x: 10, y: 20, screen_width: 640, screen_height: 400 }),
			InputEvent::MouseMoveRelative(MouseMoveRelative { x: 1, y: -1 }),
			InputEvent::MouseButtonDown(MouseButton::Left),
			InputEvent::MouseButtonUp(MouseButton::Middle),
			InputEvent::MouseScrollVertical(MouseScrollVertical { amount: 120 }),
			InputEvent::MouseScrollHorizontal(MouseScrollHorizontal { amount: -120 }),
			InputEvent::Touch(Touch { event_kind: PointerEventKind::Down, pointer_id: 3, x: 0.5, y: 0.5 }),
			InputEvent::Utf8Text("é".to_string()),
		] {
			assert_eq!(InputEvent::from_bytes(&event.to_bytes()).unwrap(), Some(event));
		}
	}

	#[test]
	fn a_gamepad_event_is_an_error_that_names_it() {
		let error = InputEvent::from_bytes(&0x5500_0004u32.to_le_bytes()).unwrap_err();
		assert!(error.to_string().contains("gamepad"), "{error}");
	}
}
