//! Mouse events. Coordinates, sizes and amounts are big-endian.

use anyhow::{Result, bail, ensure};
use strum_macros::FromRepr;

/// A pointer position in a `screen_width` × `screen_height` reference
/// frame: x (i16 BE), y (i16 BE), padding (2), width (i16 BE), height
/// (i16 BE).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseMoveAbsolute {
	pub x: i16,
	pub y: i16,
	pub screen_width: i16,
	pub screen_height: i16,
}

impl MouseMoveAbsolute {
	const SIZE: usize = 10;

	pub fn from_bytes(buffer: &[u8]) -> Result<Self> {
		ensure!(buffer.len() >= Self::SIZE, "an absolute mouse move has {} bytes, got {}", Self::SIZE, buffer.len());
		Ok(Self {
			x: i16::from_be_bytes(buffer[0..2].try_into().unwrap()),
			y: i16::from_be_bytes(buffer[2..4].try_into().unwrap()),
			screen_width: i16::from_be_bytes(buffer[6..8].try_into().unwrap()),
			screen_height: i16::from_be_bytes(buffer[8..10].try_into().unwrap()),
		})
	}

	pub fn to_bytes(&self) -> [u8; Self::SIZE] {
		let mut buffer = [0u8; Self::SIZE];
		buffer[0..2].copy_from_slice(&self.x.to_be_bytes());
		buffer[2..4].copy_from_slice(&self.y.to_be_bytes());
		buffer[6..8].copy_from_slice(&self.screen_width.to_be_bytes());
		buffer[8..10].copy_from_slice(&self.screen_height.to_be_bytes());
		buffer
	}
}

/// A pointer motion: x (i16 BE), y (i16 BE).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseMoveRelative {
	pub x: i16,
	pub y: i16,
}

impl MouseMoveRelative {
	const SIZE: usize = 4;

	pub fn from_bytes(buffer: &[u8]) -> Result<Self> {
		ensure!(buffer.len() >= Self::SIZE, "a relative mouse move has {} bytes, got {}", Self::SIZE, buffer.len());
		Ok(Self {
			x: i16::from_be_bytes(buffer[0..2].try_into().unwrap()),
			y: i16::from_be_bytes(buffer[2..4].try_into().unwrap()),
		})
	}

	pub fn to_bytes(&self) -> [u8; Self::SIZE] {
		let mut buffer = [0u8; Self::SIZE];
		buffer[0..2].copy_from_slice(&self.x.to_be_bytes());
		buffer[2..4].copy_from_slice(&self.y.to_be_bytes());
		buffer
	}
}

/// A mouse button: one byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq, FromRepr)]
#[repr(u8)]
pub enum MouseButton {
	Left = 0x01,
	Middle = 0x02,
	Right = 0x03,
	Side = 0x04,
	Extra = 0x05,
}

impl MouseButton {
	pub fn from_bytes(buffer: &[u8]) -> Result<Self> {
		ensure!(!buffer.is_empty(), "a mouse button event has 1 byte, got none");
		match MouseButton::from_repr(buffer[0]) {
			Some(button) => Ok(button),
			None => bail!("unknown mouse button {}", buffer[0]),
		}
	}

	pub fn to_bytes(self) -> [u8; 1] {
		[self as u8]
	}
}

/// A vertical wheel movement: amount (i16 BE), 120 per notch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseScrollVertical {
	pub amount: i16,
}

/// A horizontal wheel movement: amount (i16 BE), 120 per notch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseScrollHorizontal {
	pub amount: i16,
}

macro_rules! scroll_event {
	($event:ident, $name:literal) => {
		impl $event {
			pub fn from_bytes(buffer: &[u8]) -> Result<Self> {
				ensure!(buffer.len() >= 2, concat!("a ", $name, " has 2 bytes, got {}"), buffer.len());
				Ok(Self { amount: i16::from_be_bytes(buffer[0..2].try_into().unwrap()) })
			}

			pub fn to_bytes(&self) -> [u8; 2] {
				self.amount.to_be_bytes()
			}
		}
	};
}

scroll_event!(MouseScrollVertical, "vertical scroll");
scroll_event!(MouseScrollHorizontal, "horizontal scroll");

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn mouse_events_round_trip() {
		let absolute = MouseMoveAbsolute { x: 300, y: -2, screen_width: 1280, screen_height: 800 };
		assert_eq!(MouseMoveAbsolute::from_bytes(&absolute.to_bytes()).unwrap(), absolute);
		let relative = MouseMoveRelative { x: -7, y: 12 };
		assert_eq!(MouseMoveRelative::from_bytes(&relative.to_bytes()).unwrap(), relative);
		assert_eq!(MouseButton::from_bytes(&MouseButton::Right.to_bytes()).unwrap(), MouseButton::Right);
		let scroll = MouseScrollVertical { amount: -120 };
		assert_eq!(MouseScrollVertical::from_bytes(&scroll.to_bytes()).unwrap(), scroll);
	}

	#[test]
	fn short_events_and_unknown_buttons_are_errors() {
		assert!(MouseMoveAbsolute::from_bytes(&[0; 9]).is_err());
		assert!(MouseButton::from_bytes(&[9]).is_err());
	}
}
