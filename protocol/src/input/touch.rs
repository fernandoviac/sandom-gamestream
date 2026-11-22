//! Touch and pen events: 28 bytes each, little-endian. Positions are
//! fractions of the screen, 0 to 1.

use anyhow::{Result, bail, ensure};
use strum_macros::FromRepr;

const POINTER_PAYLOAD_SIZE: usize = 28;

#[derive(Debug, Clone, Copy, Eq, PartialEq, FromRepr)]
#[repr(u8)]
pub enum PointerEventKind {
	Hover = 0x00,
	Down = 0x01,
	Up = 0x02,
	Move = 0x03,
	Cancel = 0x04,
	ButtonOnly = 0x05,
	HoverLeave = 0x06,
	CancelAll = 0x07,
}

impl PointerEventKind {
	fn has_position(self) -> bool {
		matches!(self, Self::Hover | Self::Down | Self::Up | Self::Move)
	}
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, FromRepr)]
#[repr(u8)]
pub enum PenToolKind {
	Unknown = 0x00,
	Pen = 0x01,
	Eraser = 0x02,
}

/// A touch contact: event (1), padding (3), pointer id (u32 LE), x (f32
/// LE), y (f32 LE), and contact-area fields that this server does not read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
	pub event_kind: PointerEventKind,
	pub pointer_id: u32,
	pub x: f32,
	pub y: f32,
}

fn event_kind(byte: u8) -> Result<PointerEventKind> {
	match PointerEventKind::from_repr(byte) {
		Some(kind) => Ok(kind),
		None => bail!("unknown pointer event kind {byte}"),
	}
}

impl Touch {
	pub fn from_bytes(buffer: &[u8]) -> Result<Self> {
		ensure!(buffer.len() >= POINTER_PAYLOAD_SIZE, "a touch event has {POINTER_PAYLOAD_SIZE} bytes, got {}", buffer.len());
		let event_kind = event_kind(buffer[0])?;
		let x = f32::from_le_bytes(buffer[8..12].try_into().unwrap());
		let y = f32::from_le_bytes(buffer[12..16].try_into().unwrap());
		ensure!(
			!event_kind.has_position() || (x.is_finite() && y.is_finite()),
			"a touch {event_kind:?} event has the non-finite position ({x}, {y})"
		);
		Ok(Self { event_kind, pointer_id: u32::from_le_bytes(buffer[4..8].try_into().unwrap()), x, y })
	}

	pub fn to_bytes(&self) -> [u8; POINTER_PAYLOAD_SIZE] {
		let mut buffer = [0u8; POINTER_PAYLOAD_SIZE];
		buffer[0] = self.event_kind as u8;
		buffer[4..8].copy_from_slice(&self.pointer_id.to_le_bytes());
		buffer[8..12].copy_from_slice(&self.x.to_le_bytes());
		buffer[12..16].copy_from_slice(&self.y.to_le_bytes());
		buffer
	}
}

/// A pen event: event (1), tool (1), buttons (1), padding (1), x, y and
/// pressure or distance (f32 LE each), rotation (u16 LE), tilt (1), and
/// padding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pen {
	pub event_kind: PointerEventKind,
	pub tool_kind: PenToolKind,
	pub buttons: u8,
	pub x: f32,
	pub y: f32,
	pub pressure_or_distance: f32,
	pub rotation: u16,
	pub tilt: u8,
}

impl Pen {
	pub fn from_bytes(buffer: &[u8]) -> Result<Self> {
		ensure!(buffer.len() >= POINTER_PAYLOAD_SIZE, "a pen event has {POINTER_PAYLOAD_SIZE} bytes, got {}", buffer.len());
		let event_kind = event_kind(buffer[0])?;
		let tool_kind = match PenToolKind::from_repr(buffer[1]) {
			Some(kind) => kind,
			None => bail!("unknown pen tool kind {}", buffer[1]),
		};
		let x = f32::from_le_bytes(buffer[4..8].try_into().unwrap());
		let y = f32::from_le_bytes(buffer[8..12].try_into().unwrap());
		let pressure_or_distance = f32::from_le_bytes(buffer[12..16].try_into().unwrap());
		ensure!(
			!event_kind.has_position() || (x.is_finite() && y.is_finite() && pressure_or_distance.is_finite()),
			"a pen {event_kind:?} event has the non-finite values ({x}, {y}, {pressure_or_distance})"
		);
		Ok(Self {
			event_kind,
			tool_kind,
			buttons: buffer[2],
			x,
			y,
			pressure_or_distance,
			rotation: u16::from_le_bytes(buffer[16..18].try_into().unwrap()),
			tilt: buffer[18],
		})
	}

	pub fn to_bytes(&self) -> [u8; POINTER_PAYLOAD_SIZE] {
		let mut buffer = [0u8; POINTER_PAYLOAD_SIZE];
		buffer[0] = self.event_kind as u8;
		buffer[1] = self.tool_kind as u8;
		buffer[2] = self.buttons;
		buffer[4..8].copy_from_slice(&self.x.to_le_bytes());
		buffer[8..12].copy_from_slice(&self.y.to_le_bytes());
		buffer[12..16].copy_from_slice(&self.pressure_or_distance.to_le_bytes());
		buffer[16..18].copy_from_slice(&self.rotation.to_le_bytes());
		buffer[18] = self.tilt;
		buffer
	}
}

#[cfg(test)]
mod tests {
	use super::{Pen, PenToolKind, PointerEventKind, Touch};

	#[test]
	fn parses_touch_payload() {
		let mut payload = [0_u8; 28];
		payload[0] = PointerEventKind::Down as u8;
		payload[4..8].copy_from_slice(&42_u32.to_le_bytes());
		payload[8..12].copy_from_slice(&0.25_f32.to_le_bytes());
		payload[12..16].copy_from_slice(&0.75_f32.to_le_bytes());

		let touch = Touch::from_bytes(&payload).unwrap();
		assert_eq!(touch.event_kind, PointerEventKind::Down);
		assert_eq!(touch.pointer_id, 42);
		assert_eq!(touch.x, 0.25);
		assert_eq!(touch.y, 0.75);
	}

	#[test]
	fn parses_pen_payload() {
		let mut payload = [0_u8; 28];
		payload[0] = PointerEventKind::Move as u8;
		payload[1] = PenToolKind::Pen as u8;
		payload[2] = 0x02;
		payload[4..8].copy_from_slice(&0.25_f32.to_le_bytes());
		payload[8..12].copy_from_slice(&0.75_f32.to_le_bytes());
		payload[12..16].copy_from_slice(&0.6_f32.to_le_bytes());
		payload[16..18].copy_from_slice(&270_u16.to_le_bytes());
		payload[18] = 35;

		let pen = Pen::from_bytes(&payload).unwrap();
		assert_eq!(pen.event_kind, PointerEventKind::Move);
		assert_eq!(pen.tool_kind, PenToolKind::Pen);
		assert_eq!(pen.buttons, 0x02);
		assert_eq!(pen.x, 0.25);
		assert_eq!(pen.y, 0.75);
		assert_eq!(pen.pressure_or_distance, 0.6);
		assert_eq!(pen.rotation, 270);
		assert_eq!(pen.tilt, 35);
	}

	#[test]
	fn touch_and_pen_round_trip() {
		let touch = Touch { event_kind: PointerEventKind::Up, pointer_id: 7, x: 0.5, y: 0.125 };
		assert_eq!(Touch::from_bytes(&touch.to_bytes()).unwrap(), touch);
		let pen = Pen {
			event_kind: PointerEventKind::Hover,
			tool_kind: PenToolKind::Eraser,
			buttons: 1,
			x: 0.25,
			y: 0.5,
			pressure_or_distance: 0.75,
			rotation: 90,
			tilt: 12,
		};
		assert_eq!(Pen::from_bytes(&pen.to_bytes()).unwrap(), pen);
	}

	#[test]
	fn rejects_invalid_pointer_payloads() {
		assert!(Touch::from_bytes(&[0; 27]).is_err());

		let mut touch = [0_u8; 28];
		touch[0] = PointerEventKind::Move as u8;
		touch[8..12].copy_from_slice(&f32::NAN.to_le_bytes());
		assert!(Touch::from_bytes(&touch).is_err());

		let mut pen = [0_u8; 28];
		pen[0] = PointerEventKind::Move as u8;
		pen[1] = PenToolKind::Pen as u8;
		pen[12..16].copy_from_slice(&f32::INFINITY.to_le_bytes());
		assert!(Pen::from_bytes(&pen).is_err());
	}
}
