//! Input events of the control stream, from Moonshine's
//! `src/session/stream/control/input/mod.rs`.
//!
//! Sandom's changes: `InputEvent` is public and leaves the crate through
//! `ServerEvent::Input`. Gamepad events are not supported: a gamepad message
//! is reported and dropped. Moonshine's input handler, which forwarded the
//! events to its compositor and to virtual gamepads, is removed.

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

/// One input event that the client sent.
#[derive(Debug)]
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
	pub(crate) fn from_bytes(buffer: &[u8]) -> Result<Self, ()> {
		if buffer.len() < 4 {
			tracing::warn!(
				"Expected control message to have at least 4 bytes, got {}",
				buffer.len()
			);
			return Err(());
		}

		let event_type = u32::from_le_bytes(buffer[..4].try_into().unwrap());
		match InputEventType::from_repr(event_type) {
			Some(InputEventType::KeyDown) => Ok(InputEvent::KeyDown(Key::from_bytes(&buffer[4..])?)),
			Some(InputEventType::KeyUp) => Ok(InputEvent::KeyUp(Key::from_bytes(&buffer[4..])?)),
			Some(InputEventType::MouseMoveAbsolute) => Ok(InputEvent::MouseMoveAbsolute(
				MouseMoveAbsolute::from_bytes(&buffer[4..])?,
			)),
			Some(InputEventType::MouseMoveRelative) => Ok(InputEvent::MouseMoveRelative(
				MouseMoveRelative::from_bytes(&buffer[4..])?,
			)),
			Some(InputEventType::MouseButtonDown) => {
				Ok(InputEvent::MouseButtonDown(MouseButton::from_bytes(&buffer[4..])?))
			},
			Some(InputEventType::MouseButtonUp) => {
				Ok(InputEvent::MouseButtonUp(MouseButton::from_bytes(&buffer[4..])?))
			},
			Some(InputEventType::MouseScrollVertical) => Ok(InputEvent::MouseScrollVertical(
				MouseScrollVertical::from_bytes(&buffer[4..])?,
			)),
			Some(InputEventType::MouseScrollHorizontal) => Ok(InputEvent::MouseScrollHorizontal(
				MouseScrollHorizontal::from_bytes(&buffer[4..])?,
			)),
			Some(InputEventType::Touch) => Ok(InputEvent::Touch(Touch::from_bytes(&buffer[4..])?)),
			Some(InputEventType::Pen) => Ok(InputEvent::Pen(Pen::from_bytes(&buffer[4..])?)),
			Some(
				InputEventType::GamepadInfo
				| InputEventType::GamepadTouch
				| InputEventType::GamepadMotion
				| InputEventType::GamepadBattery
				| InputEventType::GamepadUpdate
				| InputEventType::GamepadEnableHaptics,
			) => {
				tracing::warn!("Dropped gamepad event 0x{event_type:08x}: this server supports no gamepads.");
				Err(())
			},
			Some(InputEventType::Utf8Text) => {
				let text = String::from_utf8(buffer[4..].to_vec())
					.map_err(|e| tracing::warn!("Invalid UTF-8 in text event: {e}"))?;
				if text.is_empty() {
					tracing::warn!("Received empty text event");
					return Err(());
				}
				Ok(InputEvent::Utf8Text(text))
			},
			None => {
				tracing::warn!("Received unknown event type: {event_type}");
				Err(())
			},
		}
	}
}

#[cfg(test)]
mod tests {
	use super::InputEvent;

	#[test]
	fn parses_utf8_text_event() {
		let payload = [0x17, 0x00, 0x00, 0x00, b'h', b'e', b'y'];
		assert!(matches!(
			InputEvent::from_bytes(&payload),
			Ok(InputEvent::Utf8Text(text)) if text == "hey"
		));
	}

	#[test]
	fn rejects_empty_utf8_text_event() {
		let payload = [0x17, 0x00, 0x00, 0x00];
		assert!(InputEvent::from_bytes(&payload).is_err());
	}
}
