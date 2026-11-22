//! The video stream's packet format, from Moonshine's
//! `src/session/stream/video/packetizer.rs`.
//!
//! A frame is its 8-byte frame header and its encoded data, cut into
//! payloads of equal size. Each payload travels in a shard: an RTP header
//! (12), 4 bytes of padding, an NV video packet header (16), and the
//! payload. The shards of a frame form up to 4 blocks; each block adds
//! Reed-Solomon parity shards that cover the whole shard. An encrypted
//! shard follows a prefix: IV (12), frame number (u32 LE), tag (16).

use anyhow::{Result, ensure};

mod assembler;
mod packetizer;
mod shard_batch;

pub use assembler::{Assembled, Frame, FrameAssembler};
pub use packetizer::Packetizer;
pub use shard_batch::ShardBatch;

/// The most shards (data and parity) of one Reed-Solomon block.
pub const MAX_SHARDS: usize = 255;
/// Two-bit block indices in the NV header's `multi_fec_blocks`.
pub const MAX_FEC_BLOCKS: usize = 4;
/// Ten-bit data count in `fec_info`; zero does not represent 1024.
pub const MAX_DATA_SHARDS_PER_BLOCK: usize = 1023;

pub const RTP_HEADER_SIZE: usize = 12;
pub const PADDING_SIZE: usize = 4;
pub const NV_VIDEO_PACKET_SIZE: usize = 16;
/// The NV header's offset in a shard.
pub const NV_PACKET_OFFSET: usize = RTP_HEADER_SIZE + PADDING_SIZE;
/// The payload's offset in a shard.
pub const PAYLOAD_OFFSET: usize = NV_PACKET_OFFSET + NV_VIDEO_PACKET_SIZE;
/// IV (12), frame number (4), tag (16).
pub const ENCRYPTION_PREFIX_SIZE: usize = 12 + 4 + 16;
pub const VIDEO_FRAME_HEADER_SIZE: usize = 8;

/// The NV header flags.
pub const FLAG_CONTAINS_PIC_DATA: u8 = 0x1;
pub const FLAG_END_OF_FRAME: u8 = 0x2;
pub const FLAG_START_OF_FRAME: u8 = 0x4;

/// The frame types of the frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
	Predicted = 1,
	Key = 2,
}

/// The header before a frame's encoded data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoFrameHeader {
	pub frame_processing_latency: u16,
	pub frame_type: FrameType,
	/// The length of the payload of the frame's last data shard.
	pub last_payload_len: u32,
}

impl VideoFrameHeader {
	const HEADER_TYPE: u8 = 0x01;

	pub fn to_bytes(&self) -> [u8; VIDEO_FRAME_HEADER_SIZE] {
		let mut buffer = [0u8; VIDEO_FRAME_HEADER_SIZE];
		buffer[0] = Self::HEADER_TYPE;
		buffer[1..3].copy_from_slice(&self.frame_processing_latency.to_le_bytes());
		buffer[3] = self.frame_type as u8;
		buffer[4..8].copy_from_slice(&self.last_payload_len.to_le_bytes());
		buffer
	}

	pub fn from_bytes(buffer: &[u8]) -> Result<Self> {
		ensure!(buffer.len() >= VIDEO_FRAME_HEADER_SIZE, "a frame header has 8 bytes, got {}", buffer.len());
		ensure!(buffer[0] == Self::HEADER_TYPE, "frame header type {} is not 1", buffer[0]);
		let frame_type = match buffer[3] {
			1 => FrameType::Predicted,
			2 => FrameType::Key,
			other => anyhow::bail!("frame type {other} is neither predicted (1) nor key (2)"),
		};
		Ok(Self {
			frame_processing_latency: u16::from_le_bytes([buffer[1], buffer[2]]),
			frame_type,
			last_payload_len: u32::from_le_bytes(buffer[4..8].try_into().unwrap()),
		})
	}
}

/// Writes an RTP header at the start of `shard`.
pub fn write_rtp_header(shard: &mut [u8], sequence_number: u16, timestamp: u32) {
	shard[0] = 0x90;
	shard[1] = 0;
	shard[2..4].copy_from_slice(&sequence_number.to_be_bytes());
	shard[4..8].copy_from_slice(&timestamp.to_be_bytes());
	shard[8..12].copy_from_slice(&0u32.to_be_bytes());
}

/// The block of `fec_info`: the shard's index in its block, the block's
/// data shard count, and the parity percentage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FecInfo {
	pub shard_index: usize,
	pub data_shards: usize,
	pub percentage: usize,
}

impl FecInfo {
	pub fn to_u32(self) -> u32 {
		(self.shard_index << 12 | self.data_shards << 22 | self.percentage << 4) as u32
	}

	pub fn from_u32(value: u32) -> Self {
		let value = value as usize;
		Self { shard_index: (value >> 12) & 0x3ff, data_shards: value >> 22, percentage: (value >> 4) & 0xff }
	}

	/// The parity shard count that a client derives:
	/// ceil(data × percentage / 100).
	pub fn parity_shards(self) -> usize {
		(self.data_shards * self.percentage).div_ceil(100)
	}
}

/// The NV video packet header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NvVideoPacket {
	pub stream_packet_index: u32,
	pub frame_index: u32,
	pub flags: u8,
	/// Current block in bits 4–5, last block in bits 6–7.
	pub multi_fec_blocks: u8,
	pub fec_info: FecInfo,
}

impl NvVideoPacket {
	const MULTI_FEC_FLAGS: u8 = 0x10;

	pub fn write(&self, buffer: &mut [u8]) {
		buffer[0..4].copy_from_slice(&self.stream_packet_index.to_le_bytes());
		buffer[4..8].copy_from_slice(&self.frame_index.to_le_bytes());
		buffer[8] = self.flags;
		buffer[9] = 0;
		buffer[10] = Self::MULTI_FEC_FLAGS;
		buffer[11] = self.multi_fec_blocks;
		buffer[12..16].copy_from_slice(&self.fec_info.to_u32().to_le_bytes());
	}

	pub fn read(buffer: &[u8]) -> Result<Self> {
		ensure!(buffer.len() >= NV_VIDEO_PACKET_SIZE, "an NV video header has 16 bytes, got {}", buffer.len());
		Ok(Self {
			stream_packet_index: u32::from_le_bytes(buffer[0..4].try_into().unwrap()),
			frame_index: u32::from_le_bytes(buffer[4..8].try_into().unwrap()),
			flags: buffer[8],
			multi_fec_blocks: buffer[11],
			fec_info: FecInfo::from_u32(u32::from_le_bytes(buffer[12..16].try_into().unwrap())),
		})
	}

	pub fn block_index(&self) -> usize {
		((self.multi_fec_blocks >> 4) & 3) as usize
	}

	pub fn last_block_index(&self) -> usize {
		(self.multi_fec_blocks >> 6) as usize
	}
}

/// The parity shard count and the wire percentage of a block of
/// `data_shards`. Moonlight derives parity as ceil(data × percentage / 100),
/// so the percentage comes first; some counts cannot be expressed by an
/// eight-bit percentage, so `minimum` is bounded by both limits.
pub fn fec_parameters(data_shards: usize, percentage: u8, minimum: u32) -> (usize, usize) {
	let max_parity = MAX_SHARDS.saturating_sub(data_shards);
	let minimum = (minimum as usize).min(max_parity);
	let minimum_percentage = if minimum == 0 { 0 } else { (minimum - 1) * 100 / data_shards + 1 };
	let max_percentage = (max_parity * 100 / data_shards).min(u8::MAX as usize);
	let percentage = (percentage as usize).max(minimum_percentage).min(max_percentage);
	((data_shards * percentage).div_ceil(100), percentage)
}

/// The IV of the `counter`th encrypted shard: the counter (u64 LE), three
/// zero bytes, and `V`.
pub fn shard_initialization_vector(counter: u64) -> [u8; 12] {
	let mut iv = [0u8; 12];
	iv[..8].copy_from_slice(&counter.to_le_bytes());
	iv[11] = b'V';
	iv
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn headers_round_trip() {
		let header = VideoFrameHeader { frame_processing_latency: 3, frame_type: FrameType::Key, last_payload_len: 77 };
		assert_eq!(VideoFrameHeader::from_bytes(&header.to_bytes()).unwrap(), header);
		let packet = NvVideoPacket {
			stream_packet_index: 5 << 8,
			frame_index: 41,
			flags: FLAG_CONTAINS_PIC_DATA | FLAG_START_OF_FRAME,
			multi_fec_blocks: (2 << 4) | (3 << 6),
			fec_info: FecInfo { shard_index: 1000, data_shards: 1023, percentage: 20 },
		};
		let mut buffer = [0u8; NV_VIDEO_PACKET_SIZE];
		packet.write(&mut buffer);
		let read = NvVideoPacket::read(&buffer).unwrap();
		assert_eq!(read, packet);
		assert_eq!((read.block_index(), read.last_block_index()), (2, 3));
	}

	#[test]
	fn parity_follows_the_advertised_percentage() {
		let (parity, percentage) = fec_parameters(213, 20, 0);
		let info = FecInfo { shard_index: 0, data_shards: 213, percentage };
		assert_eq!(info.parity_shards(), parity);
	}
}
