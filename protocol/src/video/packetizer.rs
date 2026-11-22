//! Packetizing one encoded frame into shards, from Moonshine's
//! `src/session/stream/video/packetizer.rs`.
//!
//! Sandom's changes: the caller sets the encryption key (`set_key`), and a
//! key that is not 16 bytes is refused instead of turning encryption off.
//! The Reed-Solomon encoders are built once per process and shared by every
//! session: building all block sizes takes seconds, so `warm_up` runs apart
//! from the session start, and a frame builds the encoder it lacks.

use aes_gcm::{
	Aes128Gcm, Key, Nonce,
	aead::{AeadInPlace, KeyInit},
};
use anyhow::{Result, ensure};
use fec_rs::ReedSolomon;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Instant;

/// The encoders of each (data, parity) shard count, built once.
static ENCODERS: LazyLock<Mutex<HashMap<(usize, usize), Arc<ReedSolomon>>>> = LazyLock::new(Default::default);

/// The encoder of `data_shards` and `parity_shards`, from the cache or new.
/// A new one is built without the cache's lock, so packetizing never waits
/// for another size's build.
fn shared_encoder(data_shards: usize, parity_shards: usize) -> Result<Arc<ReedSolomon>> {
	if let Some(encoder) = ENCODERS.lock().unwrap_or_else(PoisonError::into_inner).get(&(data_shards, parity_shards)) {
		return Ok(encoder.clone());
	}
	let encoder = Arc::new(ReedSolomon::new(data_shards, parity_shards).map_err(|error| {
		anyhow::anyhow!("no Reed-Solomon code of {data_shards} data and {parity_shards} parity shards: {error}")
	})?);
	let mut encoders = ENCODERS.lock().unwrap_or_else(PoisonError::into_inner);
	Ok(encoders.entry((data_shards, parity_shards)).or_insert(encoder).clone())
}

use super::shard_batch::{ShardBatch, ShardBuf};
use super::{
	ENCRYPTION_PREFIX_SIZE, FLAG_CONTAINS_PIC_DATA, FLAG_END_OF_FRAME, FLAG_START_OF_FRAME, FecInfo, FrameType,
	MAX_DATA_SHARDS_PER_BLOCK, MAX_FEC_BLOCKS, MAX_SHARDS, NV_PACKET_OFFSET, NV_VIDEO_PACKET_SIZE, NvVideoPacket,
	PAYLOAD_OFFSET, VIDEO_FRAME_HEADER_SIZE, VideoFrameHeader, fec_parameters, shard_initialization_vector,
	write_rtp_header,
};

/// Copy bytes from the logical [header ++ encoded_data] stream into a
/// destination slice, without materializing the concatenation.
fn copy_header_and_data(
	dst: &mut [u8],
	header: &[u8; VIDEO_FRAME_HEADER_SIZE],
	encoded_data: &[u8],
	offset: usize,
	len: usize,
) {
	let total = VIDEO_FRAME_HEADER_SIZE + encoded_data.len();
	let end = (offset + len).min(total);
	let mut written = 0;

	if offset < VIDEO_FRAME_HEADER_SIZE {
		let header_end = VIDEO_FRAME_HEADER_SIZE.min(end);
		let n = header_end - offset;
		dst[written..written + n].copy_from_slice(&header[offset..header_end]);
		written += n;
		if end > VIDEO_FRAME_HEADER_SIZE {
			let n = end - VIDEO_FRAME_HEADER_SIZE;
			dst[written..written + n].copy_from_slice(&encoded_data[..n]);
		}
	} else {
		let data_start = offset - VIDEO_FRAME_HEADER_SIZE;
		let data_end = end - VIDEO_FRAME_HEADER_SIZE;
		let n = data_end - data_start;
		dst[written..written + n].copy_from_slice(&encoded_data[data_start..data_end]);
	}
}

pub struct Packetizer {
	/// Whether the client enabled video encryption.
	encrypt: bool,
	/// The AES-128-GCM cipher of the last key; None until `set_key`.
	cipher: Option<Aes128Gcm>,
	/// One increment per encrypted shard; a new key starts it at 0.
	gcm_iv_counter: u64,
}

impl Packetizer {
	pub fn new(encrypt: bool) -> Self {
		Self { encrypt, cipher: None, gcm_iv_counter: 0 }
	}

	/// Encrypts later shards with `key`, the session's remote input key,
	/// when the client enabled video encryption.
	pub fn set_key(&mut self, key: &[u8]) -> Result<()> {
		if !self.encrypt {
			return Ok(());
		}
		ensure!(key.len() == 16, "a video encryption key has 16 bytes, got {}", key.len());
		self.cipher = Some(Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(key)));
		self.gcm_iv_counter = 0;
		Ok(())
	}

	/// Builds the process's encoders for every block size that carries
	/// parity at `fec_percentage`, so later frames find them built. Frames
	/// that need more than four blocks have larger blocks with less parity,
	/// so every size that can carry parity is covered. It takes seconds:
	/// run it on a thread of its own.
	pub fn warm_up(fec_percentage: u8, minimum_fec_packets: u32) -> Result<()> {
		let start = Instant::now();
		for data_shards in 1..MAX_SHARDS {
			let (parity_shards, _) = fec_parameters(data_shards, fec_percentage, minimum_fec_packets);
			if parity_shards > 0 {
				shared_encoder(data_shards, parity_shards)?;
			}
		}
		tracing::debug!("Built the Reed-Solomon encoders in {:?}.", start.elapsed());
		Ok(())
	}

	/// Packetize an encoded frame into a batch of network-ready shards.
	///
	/// Returns a `ShardBatch` containing all data + parity shards packed
	/// contiguously in a single allocation per block.
	#[allow(clippy::too_many_arguments)]
	pub fn packetize(
		&mut self,
		encoded_data: &[u8],
		frame_type: FrameType,
		requested_packet_size: usize,
		minimum_fec_packets: u32,
		fec_percentage: u8,
		frame_number: u32,
		sequence_number: &mut u32,
		rtp_timestamp: u32,
		frame_processing_latency: u16,
	) -> Result<ShardBatch> {
		ensure!(
			!self.encrypt || self.cipher.is_some(),
			"frame {frame_number} needs encryption and the packetizer has no key: call set_key first"
		);

		tracing::trace!(
			"Packetizing frame {}, size={}, type={:?}",
			frame_number,
			encoded_data.len(),
			frame_type
		);

		let requested_shard_payload_size = requested_packet_size - NV_VIDEO_PACKET_SIZE;
		let packet_data_len = VIDEO_FRAME_HEADER_SIZE + encoded_data.len();
		let last_shard_size = packet_data_len % requested_shard_payload_size;
		let last_shard_size = if last_shard_size == 0 {
			requested_shard_payload_size
		} else {
			last_shard_size
		};

		let header_bytes = VideoFrameHeader {
			frame_processing_latency,
			frame_type,
			last_payload_len: last_shard_size as u32,
		}
		.to_bytes();

		// The total size of a shard (RTP + padding + NvVideoPacket + payload).
		let requested_shard_size = PAYLOAD_OFFSET + requested_shard_payload_size;

		// When encryption is enabled, reserve space for the per-shard prefix.
		let prefix_size = if self.cipher.is_some() { ENCRYPTION_PREFIX_SIZE } else { 0 };

		let nr_data_shards = packet_data_len.div_ceil(requested_shard_payload_size);
		assert!(nr_data_shards != 0);
		ensure!(
			nr_data_shards <= MAX_FEC_BLOCKS * MAX_DATA_SHARDS_PER_BLOCK,
			"encoded frame {frame_number} needs {nr_data_shards} packets, over the wire limit of {}",
			MAX_FEC_BLOCKS * MAX_DATA_SHARDS_PER_BLOCK
		);

		// As few blocks as the percentage allows, split evenly so every block
		// gets the same protection (RFC 5052, section 9.1). A frame that needs
		// more than four blocks gets four larger ones, each with the parity that
		// still fits, and none once a block is too large for any.
		let data_shards_per_block = MAX_SHARDS * 100 / (100 + fec_percentage as usize);
		let nr_blocks = nr_data_shards.div_ceil(data_shards_per_block).min(MAX_FEC_BLOCKS);
		let (shards_per_block, larger_blocks) = (nr_data_shards / nr_blocks, nr_data_shards % nr_blocks);
		// Last block occupies bits 6-7, current block bits 4-5.
		let last_block_index = (nr_blocks as u8 - 1) << 6;

		tracing::trace!("Sending {nr_blocks} blocks of video data.");

		// Accumulate all blocks into a single batch.
		let mut all_shards = ShardBatch::empty();

		let mut total_alloc_us = 0u128;
		let mut total_data_write_us = 0u128;
		let mut total_fec_encoder_us = 0u128;
		let mut total_fec_compute_us = 0u128;
		let mut total_fec_headers_us = 0u128;
		let mut total_extend_us = 0u128;

		for block_index in 0..nr_blocks {
			let start = block_index * shards_per_block + block_index.min(larger_blocks);
			let end = start + shards_per_block + usize::from(block_index < larger_blocks);

			let nr_data_shards = end - start;
			assert!(nr_data_shards != 0);

			let (nr_parity_shards, fec_percentage) =
				fec_parameters(nr_data_shards, fec_percentage, minimum_fec_packets);

			let t_fec_encoder = Instant::now();
			let encoder = if nr_parity_shards > 0 {
				Some(shared_encoder(nr_data_shards, nr_parity_shards)?)
			} else {
				None
			};
			total_fec_encoder_us += t_fec_encoder.elapsed().as_micros();

			tracing::trace!(
				"Sending block {block_index} with {nr_data_shards} data shards and {nr_parity_shards} parity shards."
			);

			// Single allocation for all shards in this block (data + parity), zeroed.
			let total_shards = nr_data_shards + nr_parity_shards;
			let t_alloc = Instant::now();
			let mut shard_buf = ShardBuf::new(total_shards, requested_shard_size, prefix_size);
			total_alloc_us += t_alloc.elapsed().as_micros();

			let t_data_write = Instant::now();

			// Write data shards directly into the flat buffer.
			for (block_shard_index, data_shard_index) in (start..end).enumerate() {
				let payload_start = data_shard_index * requested_shard_payload_size;
				let payload_len = requested_shard_payload_size.min(packet_data_len - payload_start);

				let shard = shard_buf.shard_mut(block_shard_index);

				// Write RTP header.
				write_rtp_header(shard, *sequence_number as u16, rtp_timestamp);

				// Padding (4 bytes of zeros) is already zeroed.

				// Write NvVideoPacket header.
				let mut flags = FLAG_CONTAINS_PIC_DATA;
				if block_shard_index == 0 {
					flags |= FLAG_START_OF_FRAME;
				}
				if block_shard_index == nr_data_shards - 1 {
					flags |= FLAG_END_OF_FRAME;
				}
				NvVideoPacket {
					stream_packet_index: *sequence_number << 8,
					frame_index: frame_number,
					flags,
					multi_fec_blocks: ((block_index as u8) << 4) | last_block_index,
					fec_info: FecInfo { shard_index: block_shard_index, data_shards: nr_data_shards, percentage: fec_percentage },
				}
				.write(&mut shard[NV_PACKET_OFFSET..NV_PACKET_OFFSET + NV_VIDEO_PACKET_SIZE]);

				// Copy payload from [header ++ encoded_data].
				copy_header_and_data(
					&mut shard[PAYLOAD_OFFSET..],
					&header_bytes,
					encoded_data,
					payload_start,
					payload_len,
				);

				// Remaining bytes are already zero (padding for undersized last shard).

				*sequence_number += 1;
			}

			// Parity shards are already zeroed from ShardBuf::new().

			total_data_write_us += t_data_write.elapsed().as_micros();

			if let Some(encoder) = encoder {
				// Create FEC-compatible slice views into the flat buffer.
				let mut fec_slices = shard_buf.as_fec_slices();

				let t_fec_compute = Instant::now();

				encoder
					.encode(&mut fec_slices)
					.map_err(|e| anyhow::anyhow!("Reed-Solomon encoding of frame {frame_number} failed: {e}"))?;

				total_fec_compute_us += t_fec_compute.elapsed().as_micros();

				let t_fec_headers = Instant::now();

				// Write headers for parity shards. FEC overwrites the entire shard
				// content, so we patch the fields Moonlight needs afterward.
				for block_shard_index in 0..nr_parity_shards {
					let shard = shard_buf.shard_mut(nr_data_shards + block_shard_index);

					// RTP header.
					shard[0] = 0x90;
					shard[1] = 0; // packet_type
					shard[2..4].copy_from_slice(&(*sequence_number as u16).to_be_bytes());

					// NvVideoPacket fields that Moonlight needs.
					let nv = &mut shard[NV_PACKET_OFFSET..NV_PACKET_OFFSET + NV_VIDEO_PACKET_SIZE];
					nv[4..8].copy_from_slice(&frame_number.to_le_bytes()); // frame_index
					nv[11] = ((block_index as u8) << 4) | last_block_index; // multi_fec_blocks
					let fec_info = FecInfo {
						shard_index: nr_data_shards + block_shard_index,
						data_shards: nr_data_shards,
						percentage: fec_percentage,
					};
					nv[12..16].copy_from_slice(&fec_info.to_u32().to_le_bytes()); // fec_info

					*sequence_number += 1;
				}

				total_fec_headers_us += t_fec_headers.elapsed().as_micros();
			}

			// Encrypt each shard if video encryption is enabled.
			if let Some(cipher) = &self.cipher {
				for shard_index in 0..total_shards {
					let iv = shard_initialization_vector(self.gcm_iv_counter);
					self.gcm_iv_counter += 1;

					let nonce = Nonce::from_slice(&iv);

					// Encrypt the shard data in-place, returning a detached 16-byte tag.
					let shard_data = shard_buf.shard_mut(shard_index);
					let tag = cipher
						.encrypt_in_place_detached(nonce, b"", shard_data)
						.map_err(|_| anyhow::anyhow!("AES-GCM encryption of a shard of frame {frame_number} failed"))?;

					// Fill the encryption prefix: iv(12) + frameNumber(4) + tag(16).
					let prefix = shard_buf.prefix_mut(shard_index);
					prefix[..12].copy_from_slice(&iv);
					prefix[12..16].copy_from_slice(&frame_number.to_le_bytes());
					prefix[16..32].copy_from_slice(&tag);
				}
			}

			let t_extend = Instant::now();
			all_shards.extend(shard_buf.into_batch());
			total_extend_us += t_extend.elapsed().as_micros();

			tracing::trace!("Finished sending frame {frame_number}.");
		}

		tracing::trace!(
			"Packetize breakdown: alloc_us={total_alloc_us} data_write_us={total_data_write_us} fec_encoder_us={total_fec_encoder_us} fec_compute_us={total_fec_compute_us} fec_headers_us={total_fec_headers_us} extend_us={total_extend_us}",
		);

		Ok(all_shards)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::BTreeMap;

	fn packetizer() -> Packetizer {
		Packetizer::new(false)
	}

	/// A frame that exactly fills `data_shards` packets of 80 bytes.
	fn frame(data_shards: usize) -> Vec<u8> {
		(0..data_shards * 64 - VIDEO_FRAME_HEADER_SIZE)
			.map(|index| (index % 251) as u8)
			.collect()
	}

	/// The packets of each block, data first, in block order.
	fn blocks(batch: &ShardBatch) -> Vec<Vec<&[u8]>> {
		let mut blocks = BTreeMap::<u8, Vec<&[u8]>>::new();
		for packet in batch.as_bytes().chunks_exact(batch.shard_size()) {
			blocks.entry((packet[27] >> 4) & 3).or_default().push(packet);
		}
		blocks.into_values().collect()
	}

	/// A block's data count and the parity Moonlight derives from its advertised percentage.
	fn counts(packet: &[u8]) -> (usize, usize) {
		let fec_info = u32::from_le_bytes(packet[28..32].try_into().unwrap()) as usize;
		let (data, percentage) = (fec_info >> 22, (fec_info >> 4) & 0xff);
		(data, (data * percentage).div_ceil(100))
	}

	#[test]
	fn frames_are_split_evenly_across_blocks() {
		for (shards, layout) in [
			(600, vec![(200, 40); 3]),
			(900, vec![(225, 30); 4]),
			(1009, vec![(253, 0), (252, 3), (252, 3), (252, 3)]),
			(4092, vec![(1023, 0); 4]),
		] {
			let data = frame(shards);
			let batch = packetizer()
				.packetize(&data, FrameType::Key, 80, 2, 20, 1, &mut 0, 0, 0)
				.unwrap();
			let blocks = blocks(&batch);
			let wire: Vec<_> = blocks.iter().map(|block| counts(block[0])).collect();
			assert_eq!(wire, layout, "{shards} data packets");
			let sent: Vec<u8> = blocks
				.iter()
				.flat_map(|block| &block[..counts(block[0]).0])
				.flat_map(|packet| &packet[PAYLOAD_OFFSET..])
				.copied()
				.collect();
			assert!(sent[VIDEO_FRAME_HEADER_SIZE..] == data[..], "{shards} data packets");
		}
	}

	#[test]
	fn oversized_frame_does_not_consume_sequence_numbers() {
		let mut sequence = 77;
		let result = packetizer().packetize(&frame(4093), FrameType::Key, 80, 2, 20, 1, &mut sequence, 0, 0);
		assert!(result.is_err());
		assert_eq!(sequence, 77);
	}

	#[test]
	fn parity_matches_the_advertised_percentage() {
		for (data_shards, percentage, minimum) in
			[(213, 20, 0), (1, 20, 2), (40, 0, 2), (40, 20, u32::MAX), (600, 100, 2)]
		{
			let batch = packetizer()
				.packetize(
					&frame(data_shards),
					FrameType::Key,
					80,
					minimum,
					percentage,
					1,
					&mut 0,
					0,
					0,
				)
				.unwrap();
			for block in blocks(&batch) {
				let (data, parity) = counts(block[0]);
				assert_eq!(
					block.len(),
					data + parity,
					"{data_shards} data packets, minimum {minimum}"
				);
				assert!(parity > 0, "{data_shards} data packets, minimum {minimum}");
			}
		}
	}
}
