//! Reassembling frames from received shards: the client's side of
//! `Packetizer`.
//!
//! Each datagram is one shard. The assembler decrypts it when the session
//! encrypts video, files it under its frame and block, and completes a
//! block when the block holds all its data shards, or when it holds as many
//! shards as data shards and Reed-Solomon recovers the missing ones. Parity
//! covers whole shards, but the server rewrites some header bytes of its
//! parity shards after the encoding, so only the payload of a recovered
//! shard is exact; the assembler reads only payloads of recovered shards.
//! A frame completes when all its blocks do.

use std::collections::{BTreeMap, HashMap};

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Key, Nonce, Tag};
use anyhow::{Context, Result, bail, ensure};
use fec_rs::ReedSolomon;

use super::{
	ENCRYPTION_PREFIX_SIZE, FrameType, MAX_FEC_BLOCKS, NV_PACKET_OFFSET, NV_VIDEO_PACKET_SIZE, NvVideoPacket,
	PAYLOAD_OFFSET, VIDEO_FRAME_HEADER_SIZE, VideoFrameHeader,
};

/// One reassembled frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
	pub frame_index: u32,
	pub frame_type: FrameType,
	/// The 90 kHz RTP timestamp, from a received data shard; None when every
	/// data shard of the frame was recovered from parity.
	pub rtp_timestamp: Option<u32>,
	/// The encoded data: one H.264 access unit.
	pub data: Vec<u8>,
}

/// What one datagram completed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Assembled {
	/// The frames that the datagram completed, in frame order.
	pub frames: Vec<Frame>,
	/// The frames that a later frame overtook before they completed: the
	/// decoder has lost a reference, and the client asks for a key frame.
	pub lost: Vec<u32>,
}

struct Block {
	data_shards: usize,
	shards: Vec<Option<Vec<u8>>>,
	/// Whether each data shard arrived (true) or is to be recovered.
	received: Vec<bool>,
	received_count: usize,
	/// The payloads of the data shards, once the block is complete.
	payloads: Option<Vec<u8>>,
}

struct PendingFrame {
	blocks: Vec<Option<Block>>,
	rtp_timestamp: Option<u32>,
}

/// The frames of one session's video stream.
pub struct FrameAssembler {
	cipher: Option<Aes128Gcm>,
	pending: BTreeMap<u32, PendingFrame>,
	/// The last frame delivered or given up; older shards are late.
	last_frame: Option<u32>,
	decoders: HashMap<(usize, usize), ReedSolomon>,
}

impl FrameAssembler {
	/// An assembler of a stream that `key` encrypts, or of a plain stream.
	pub fn new(key: Option<&[u8]>) -> Result<Self> {
		let cipher = match key {
			Some(key) => {
				ensure!(key.len() == 16, "a video encryption key has 16 bytes, got {}", key.len());
				Some(Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(key)))
			},
			None => None,
		};
		Ok(Self { cipher, pending: BTreeMap::new(), last_frame: None, decoders: HashMap::new() })
	}

	/// Files one datagram and returns what it completed. A shard of a frame
	/// at or before the last delivered frame is late, and changes nothing.
	pub fn push(&mut self, datagram: &[u8]) -> Result<Assembled> {
		let shard = match &self.cipher {
			Some(cipher) => decrypt_shard(cipher, datagram)?,
			None => datagram.to_vec(),
		};
		ensure!(shard.len() > PAYLOAD_OFFSET, "a video shard has more than {PAYLOAD_OFFSET} bytes, got {}", shard.len());
		let header = NvVideoPacket::read(&shard[NV_PACKET_OFFSET..NV_PACKET_OFFSET + NV_VIDEO_PACKET_SIZE])?;
		let frame_index = header.frame_index;
		if self.last_frame.is_some_and(|last| frame_index <= last) {
			return Ok(Assembled::default());
		}
		let (block_index, last_block_index) = (header.block_index(), header.last_block_index());
		ensure!(
			block_index <= last_block_index && last_block_index < MAX_FEC_BLOCKS,
			"frame {frame_index} names block {block_index} of {} blocks",
			last_block_index + 1
		);
		let info = header.fec_info;
		let parity_shards = info.parity_shards();
		ensure!(info.data_shards > 0, "frame {frame_index} block {block_index} has no data shards");
		ensure!(
			info.shard_index < info.data_shards + parity_shards,
			"shard {} of frame {frame_index} block {block_index} is outside its {} data and {parity_shards} parity shards",
			info.shard_index,
			info.data_shards
		);

		let frame = self
			.pending
			.entry(frame_index)
			.or_insert_with(|| PendingFrame { blocks: (0..=last_block_index).map(|_| None).collect(), rtp_timestamp: None });
		ensure!(
			frame.blocks.len() == last_block_index + 1,
			"frame {frame_index} names {} blocks and earlier shards named {}",
			last_block_index + 1,
			frame.blocks.len()
		);
		let block = frame.blocks[block_index].get_or_insert_with(|| Block {
			data_shards: info.data_shards,
			shards: vec![None; info.data_shards + parity_shards],
			received: vec![false; info.data_shards],
			received_count: 0,
			payloads: None,
		});
		// A completed block keeps only its payloads: its later shards, such
		// as the parity after all data arrived, change nothing.
		if block.payloads.is_some() {
			return Ok(Assembled::default());
		}
		ensure!(
			block.data_shards == info.data_shards && block.shards.len() == info.data_shards + parity_shards,
			"frame {frame_index} block {block_index} changed its shard counts"
		);
		if block.shards[info.shard_index].is_some() {
			return Ok(Assembled::default());
		}
		if info.shard_index < info.data_shards {
			block.received[info.shard_index] = true;
			if frame.rtp_timestamp.is_none() {
				frame.rtp_timestamp = Some(u32::from_be_bytes(shard[4..8].try_into().unwrap()));
			}
		}
		block.shards[info.shard_index] = Some(shard);
		block.received_count += 1;

		let complete = block.received.iter().all(|&received| received);
		if complete || block.received_count >= block.data_shards {
			if !complete {
				let decoder = match self.decoders.entry((block.data_shards, parity_shards)) {
					std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
					std::collections::hash_map::Entry::Vacant(entry) => {
						entry.insert(ReedSolomon::new(block.data_shards, parity_shards).map_err(|error| {
							anyhow::anyhow!(
								"no Reed-Solomon code of {} data and {parity_shards} parity shards: {error}",
								block.data_shards
							)
						})?)
					},
				};
				decoder
					.reconstruct_data(&mut block.shards)
					.map_err(|error| anyhow::anyhow!("recovering frame {frame_index} block {block_index} failed: {error}"))?;
			}
			let mut payloads = Vec::new();
			for shard in &block.shards[..block.data_shards] {
				payloads.extend_from_slice(&shard.as_ref().expect("a complete block has its data shards")[PAYLOAD_OFFSET..]);
			}
			block.payloads = Some(payloads);
			block.shards = Vec::new();
		}

		if frame.blocks.iter().any(|block| block.as_ref().is_none_or(|block| block.payloads.is_none())) {
			return Ok(Assembled::default());
		}
		let frame = self.pending.remove(&frame_index).expect("the frame is pending");
		let lost: Vec<u32> = self.pending.range(..frame_index).map(|(&index, _)| index).collect();
		for index in &lost {
			self.pending.remove(index);
		}
		self.last_frame = Some(frame_index);
		Ok(Assembled { frames: vec![finish(frame_index, frame)?], lost })
	}
}

fn decrypt_shard(cipher: &Aes128Gcm, datagram: &[u8]) -> Result<Vec<u8>> {
	ensure!(
		datagram.len() > ENCRYPTION_PREFIX_SIZE,
		"an encrypted video shard has more than {ENCRYPTION_PREFIX_SIZE} bytes, got {}",
		datagram.len()
	);
	let (prefix, ciphertext) = datagram.split_at(ENCRYPTION_PREFIX_SIZE);
	let mut shard = ciphertext.to_vec();
	cipher
		.decrypt_in_place_detached(Nonce::from_slice(&prefix[..12]), b"", &mut shard, Tag::from_slice(&prefix[16..32]))
		.map_err(|_| anyhow::anyhow!("a video shard failed AES-GCM authentication: wrong key or altered data"))?;
	Ok(shard)
}

fn finish(frame_index: u32, frame: PendingFrame) -> Result<Frame> {
	let mut bytes = Vec::new();
	let mut payload_size = None;
	let mut data_shards = 0;
	for block in frame.blocks {
		let block = block.expect("a finished frame has every block");
		let payloads = block.payloads.expect("a finished frame has every block's payloads");
		payload_size.get_or_insert(payloads.len() / block.data_shards);
		data_shards += block.data_shards;
		bytes.extend(payloads);
	}
	let payload_size = payload_size.context("a frame has no blocks")?;
	let header = VideoFrameHeader::from_bytes(&bytes).with_context(|| format!("frame {frame_index}"))?;
	let last_payload_len = header.last_payload_len as usize;
	if last_payload_len == 0 || last_payload_len > payload_size {
		bail!("frame {frame_index} states a last payload of {last_payload_len} bytes in {payload_size}-byte payloads");
	}
	let length = (data_shards - 1) * payload_size + last_payload_len;
	ensure!(
		length >= VIDEO_FRAME_HEADER_SIZE,
		"frame {frame_index} is {length} bytes, shorter than its header"
	);
	bytes.truncate(length);
	bytes.drain(..VIDEO_FRAME_HEADER_SIZE);
	Ok(Frame { frame_index, frame_type: header.frame_type, rtp_timestamp: frame.rtp_timestamp, data: bytes })
}

#[cfg(test)]
mod tests {
	use super::super::{FrameType, Packetizer, ShardBatch};
	use super::*;

	const PACKET_SIZE: usize = 80;

	fn frame_data(length: usize, seed: u8) -> Vec<u8> {
		(0..length).map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed)).collect()
	}

	fn packetize(packetizer: &mut Packetizer, data: &[u8], frame_index: u32, sequence: &mut u32) -> ShardBatch {
		packetizer.packetize(data, FrameType::Key, PACKET_SIZE, 2, 20, frame_index, sequence, 9000, 0).unwrap()
	}

	fn shards(batch: &ShardBatch) -> Vec<&[u8]> {
		batch.as_bytes().chunks_exact(batch.shard_size()).collect()
	}

	/// The data and parity shards of each block, from the plain shards.
	fn block_layout(shards: &[&[u8]]) -> Vec<(usize, usize, Vec<usize>)> {
		let mut blocks: BTreeMap<usize, (usize, usize, Vec<usize>)> = BTreeMap::new();
		for (position, shard) in shards.iter().enumerate() {
			let header = NvVideoPacket::read(&shard[NV_PACKET_OFFSET..]).unwrap();
			let entry = blocks.entry(header.block_index()).or_insert((
				header.fec_info.data_shards,
				header.fec_info.parity_shards(),
				Vec::new(),
			));
			entry.2.push(position);
		}
		blocks.into_values().collect()
	}

	#[test]
	fn every_frame_arrives_whole_without_loss() {
		let mut packetizer = Packetizer::new(false);
		let mut assembler = FrameAssembler::new(None).unwrap();
		let mut sequence = 0;
		for (frame_index, length) in [(1, 10), (2, 700), (3, 64 * 600)] {
			let data = frame_data(length, frame_index as u8);
			let batch = packetize(&mut packetizer, &data, frame_index, &mut sequence);
			let mut frames = Vec::new();
			for shard in shards(&batch) {
				frames.extend(assembler.push(shard).unwrap().frames);
			}
			assert_eq!(frames.len(), 1, "frame {frame_index}");
			assert_eq!(frames[0].data, data, "frame {frame_index}");
			assert_eq!(frames[0].frame_type, FrameType::Key);
			assert_eq!(frames[0].rtp_timestamp, Some(9000));
		}
	}

	#[test]
	fn a_frame_survives_the_loss_of_as_many_shards_as_its_parity() {
		// 600 data shards: three blocks of 200 data and 40 parity shards.
		let data = frame_data(64 * 600 - VIDEO_FRAME_HEADER_SIZE, 7);
		for drop_from in ["the start", "the end"] {
			let mut packetizer = Packetizer::new(false);
			let batch = packetize(&mut packetizer, &data, 1, &mut 0);
			let all = shards(&batch);
			let mut dropped = std::collections::HashSet::new();
			for (data_shards, parity_shards, positions) in block_layout(&all) {
				assert!(parity_shards > 0);
				let data_positions = &positions[..data_shards];
				let lost: Vec<usize> = match drop_from {
					"the start" => data_positions[..parity_shards].to_vec(),
					_ => data_positions[data_shards - parity_shards..].to_vec(),
				};
				dropped.extend(lost);
			}
			let mut assembler = FrameAssembler::new(None).unwrap();
			let mut frames = Vec::new();
			for (position, shard) in all.iter().enumerate() {
				if !dropped.contains(&position) {
					frames.extend(assembler.push(shard).unwrap().frames);
				}
			}
			assert_eq!(frames.len(), 1, "dropping data shards from {drop_from}");
			assert_eq!(frames[0].data, data, "dropping data shards from {drop_from}");
		}
	}

	#[test]
	fn one_shard_more_than_the_parity_loses_the_frame_and_the_next_frame_reports_it() {
		let data = frame_data(64 * 100, 3);
		let mut packetizer = Packetizer::new(false);
		let mut sequence = 0;
		let first = packetize(&mut packetizer, &data, 1, &mut sequence);
		let second = packetize(&mut packetizer, &data, 2, &mut sequence);
		let first_shards = shards(&first);
		let (_, parity_shards, positions) = block_layout(&first_shards).remove(0);
		let mut assembler = FrameAssembler::new(None).unwrap();
		for (position, shard) in first_shards.iter().enumerate() {
			if !positions[..=parity_shards].contains(&position) {
				assert!(assembler.push(shard).unwrap().frames.is_empty());
			}
		}
		let mut assembled = Assembled::default();
		for shard in shards(&second) {
			let result = assembler.push(shard).unwrap();
			assembled.frames.extend(result.frames);
			assembled.lost.extend(result.lost);
		}
		assert_eq!(assembled.lost, vec![1]);
		assert_eq!(assembled.frames.len(), 1);
		assert_eq!(assembled.frames[0].frame_index, 2);
		// A late shard of the lost frame changes nothing.
		assert_eq!(assembler.push(first_shards[0]).unwrap(), Assembled::default());
	}

	#[test]
	fn an_encrypted_stream_needs_its_key() {
		let key = [0x42u8; 16];
		let data = frame_data(500, 9);
		let mut packetizer = Packetizer::new(true);
		packetizer.set_key(&key).unwrap();
		let batch = packetize(&mut packetizer, &data, 1, &mut 0);
		let mut assembler = FrameAssembler::new(Some(&key)).unwrap();
		let mut frames = Vec::new();
		for shard in shards(&batch) {
			frames.extend(assembler.push(shard).unwrap().frames);
		}
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].data, data);
		let mut wrong = FrameAssembler::new(Some(&[0x43; 16])).unwrap();
		assert!(wrong.push(shards(&batch)[0]).is_err());
	}
}
