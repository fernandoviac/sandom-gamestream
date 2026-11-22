//! Decoding H.264 access units with VideoToolbox into Metal textures, on
//! macOS.
//!
//! A GameStream server sends each access unit in Annex B form: NAL units
//! after start codes, with the sequence and picture parameter sets before
//! each key frame. The decoder builds its format description from the
//! parameter sets, gives VideoToolbox the other NAL units with 4-byte
//! lengths, and asks for BGRA pixel buffers that live in IOSurfaces, so a
//! Metal texture of the same memory shows the picture without a copy.

use std::ffi::c_void;
use std::time::Instant;

use anyhow::{Result, bail, ensure};

use crate::apple::*;
use crate::h264::{NAL_ACCESS_UNIT_DELIMITER, NAL_PICTURE_PARAMETER_SET, NAL_SEQUENCE_PARAMETER_SET, nal_units};
use sandom_gamestream_protocol::video::Frame;

/// One decoded picture: a BGRA Metal texture and the pixel buffer behind
/// it. The texture stays valid, and the decoder does not reuse its memory,
/// while the picture lives.
pub struct Picture {
	texture: CVMetalTextureRef,
	pixels: CVPixelBufferRef,
	pub width: u32,
	pub height: u32,
	pub frame_index: u32,
	/// The frame's 90 kHz RTP timestamp, when a data shard carried it.
	pub rtp_timestamp: Option<u32>,
	/// When the decoder finished the picture.
	pub decoded_at: Instant,
}

// The texture and pixel buffer are Core Foundation and Metal objects, which
// any thread may retain, release and read.
unsafe impl Send for Picture {}
unsafe impl Sync for Picture {}

impl std::fmt::Debug for Picture {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("Picture")
			.field("width", &self.width)
			.field("height", &self.height)
			.field("frame_index", &self.frame_index)
			.field("rtp_timestamp", &self.rtp_timestamp)
			.finish()
	}
}

impl Picture {
	/// The `id<MTLTexture>`, BGRA8Unorm, valid while this picture lives.
	pub fn metal_texture(&self) -> *mut c_void {
		unsafe { CVMetalTextureGetTexture(self.texture) }
	}

	/// The BGRA bytes, row by row without padding: a CPU read of the
	/// IOSurface, for inspection and tests.
	pub fn read_bgra(&self) -> Result<Vec<u8>> {
		let status = unsafe { CVPixelBufferLockBaseAddress(self.pixels, 1) };
		ensure!(status == 0, "Core Video could not lock the decoded picture: status {status}");
		let (width, height) = (self.width as usize, self.height as usize);
		let mut bytes = Vec::with_capacity(width * height * 4);
		unsafe {
			let base = CVPixelBufferGetBaseAddress(self.pixels);
			let stride = CVPixelBufferGetBytesPerRow(self.pixels);
			for row in 0..height {
				bytes.extend_from_slice(std::slice::from_raw_parts(base.add(row * stride), width * 4));
			}
			CVPixelBufferUnlockBaseAddress(self.pixels, 1);
		}
		Ok(bytes)
	}
}

impl Drop for Picture {
	fn drop(&mut self) {
		unsafe {
			CFRelease(self.texture);
			CFRelease(self.pixels);
		}
	}
}

/// What the output callback of one synchronous decode wrote.
struct Output {
	status: OSStatus,
	image: CVPixelBufferRef,
}

extern "C" fn decoded(
	context: *mut c_void,
	_frame: *mut c_void,
	status: OSStatus,
	_flags: u32,
	image: CVPixelBufferRef,
	_presentation: CMTime,
	_duration: CMTime,
) {
	let output = unsafe { &mut *(context as *mut Output) };
	output.status = status;
	if status == 0 && !image.is_null() {
		output.image = unsafe { CFRetain(image) } as CVPixelBufferRef;
	}
}

/// A decoder of one session's stream.
pub struct Decoder {
	device: MTLObject,
	textures: CVMetalTextureCacheRef,
	session: VTDecompressionSessionRef,
	format: Option<Owned>,
	parameter_sets: Option<(Vec<u8>, Vec<u8>)>,
	/// Boxed, so its address stays the callback's context.
	output: Box<Output>,
}

// The decoder's objects are Core Foundation and Metal objects; one thread at
// a time uses the decoder.
unsafe impl Send for Decoder {}

impl Decoder {
	/// A decoder whose textures belong to `device` (an `id<MTLDevice>`), or
	/// to the system's default Metal device when `device` is null.
	pub fn new(device: *mut c_void) -> Result<Self> {
		let device = if device.is_null() { unsafe { MTLCreateSystemDefaultDevice() } } else { unsafe { CFRetain(device) as MTLObject } };
		ensure!(!device.is_null(), "this Mac has no Metal device");
		let mut textures = std::ptr::null_mut();
		let status =
			unsafe { CVMetalTextureCacheCreate(std::ptr::null(), std::ptr::null(), device, std::ptr::null(), &mut textures) };
		if status != 0 || textures.is_null() {
			unsafe { CFRelease(device) };
			bail!("Core Video created no Metal texture cache: status {status}");
		}
		Ok(Self {
			device,
			textures,
			session: std::ptr::null_mut(),
			format: None,
			parameter_sets: None,
			output: Box::new(Output { status: 0, image: std::ptr::null_mut() }),
		})
	}

	/// Decodes one access unit into a picture.
	pub fn decode(&mut self, frame: &Frame) -> Result<Picture> {
		let mut sequence = None;
		let mut picture = None;
		let mut slices = Vec::with_capacity(frame.data.len() + 16);
		for unit in nal_units(&frame.data) {
			match unit[0] & 0x1f {
				NAL_SEQUENCE_PARAMETER_SET => sequence = Some(unit),
				NAL_PICTURE_PARAMETER_SET => picture = Some(unit),
				NAL_ACCESS_UNIT_DELIMITER => {},
				_ => {
					slices.extend((unit.len() as u32).to_be_bytes());
					slices.extend_from_slice(unit);
				},
			}
		}
		match (sequence, picture) {
			(Some(sequence), Some(picture)) => self.set_parameter_sets(sequence, picture)?,
			(None, None) => {},
			_ => bail!("frame {} carries only one of the sequence and picture parameter sets", frame.frame_index),
		}
		ensure!(
			!self.session.is_null(),
			"frame {} arrived before any key frame with parameter sets; the stream starts with a key frame",
			frame.frame_index
		);
		ensure!(!slices.is_empty(), "frame {} has no picture data", frame.frame_index);
		self.decode_slices(frame, &mut slices)
	}

	fn set_parameter_sets(&mut self, sequence: &[u8], picture: &[u8]) -> Result<()> {
		if self.parameter_sets.as_ref().is_some_and(|(s, p)| s == sequence && p == picture) {
			return Ok(());
		}
		let pointers = [sequence.as_ptr(), picture.as_ptr()];
		let sizes = [sequence.len(), picture.len()];
		let mut format = std::ptr::null_mut();
		let status = unsafe {
			CMVideoFormatDescriptionCreateFromH264ParameterSets(std::ptr::null(), 2, pointers.as_ptr(), sizes.as_ptr(), 4, &mut format)
		};
		ensure!(status == 0 && !format.is_null(), "Core Media refused the stream's parameter sets: status {status}");
		let format = Owned(format);
		let accepted = !self.session.is_null()
			&& unsafe { VTDecompressionSessionCanAcceptFormatDescription(self.session, format.0 as CMFormatDescriptionRef) } != 0;
		if !accepted {
			self.close_session();
			self.session = self.create_session(format.0 as CMFormatDescriptionRef)?;
		}
		self.format = Some(format);
		self.parameter_sets = Some((sequence.to_vec(), picture.to_vec()));
		Ok(())
	}

	fn create_session(&mut self, format: CMFormatDescriptionRef) -> Result<VTDecompressionSessionRef> {
		let attributes = Owned::dictionary()?;
		let pixel_format = Owned::number(kCVPixelFormatType_32BGRA as i32)?;
		let surface = Owned::dictionary()?;
		unsafe {
			attributes.set(kCVPixelBufferPixelFormatTypeKey, pixel_format.0);
			attributes.set(kCVPixelBufferMetalCompatibilityKey, kCFBooleanTrue);
			attributes.set(kCVPixelBufferIOSurfacePropertiesKey, surface.0);
		}
		let record = VTDecompressionOutputCallbackRecord {
			callback: decoded,
			context: (&mut *self.output as *mut Output).cast(),
		};
		let mut session = std::ptr::null_mut();
		let status = unsafe {
			VTDecompressionSessionCreate(std::ptr::null(), format, std::ptr::null(), attributes.0, &record, &mut session)
		};
		ensure!(status == 0 && !session.is_null(), "VideoToolbox created no H.264 decoder: status {status}");
		Ok(session)
	}

	fn decode_slices(&mut self, frame: &Frame, slices: &mut [u8]) -> Result<Picture> {
		let mut block = std::ptr::null_mut();
		let status = unsafe {
			CMBlockBufferCreateWithMemoryBlock(
				std::ptr::null(),
				slices.as_mut_ptr().cast(),
				slices.len(),
				kCFAllocatorNull,
				std::ptr::null(),
				0,
				slices.len(),
				0,
				&mut block,
			)
		};
		ensure!(status == 0 && !block.is_null(), "Core Media created no block buffer for frame {}: status {status}", frame.frame_index);
		let block = Owned(block);
		let presentation = CMTime {
			value: i64::from(frame.rtp_timestamp.unwrap_or(0)),
			timescale: 90_000,
			flags: CMTIME_FLAGS_VALID,
			epoch: 0,
		};
		let timing = CMSampleTimingInfo { duration: CMTIME_INVALID, presentation_time_stamp: presentation, decode_time_stamp: CMTIME_INVALID };
		let size = slices.len();
		let mut sample = std::ptr::null_mut();
		let status = unsafe {
			CMSampleBufferCreateReady(
				std::ptr::null(),
				block.0 as CMBlockBufferRef,
				self.format.as_ref().expect("a session has a format").0 as CMFormatDescriptionRef,
				1,
				1,
				&timing,
				1,
				&size,
				&mut sample,
			)
		};
		ensure!(status == 0 && !sample.is_null(), "Core Media created no sample for frame {}: status {status}", frame.frame_index);
		let sample = Owned(sample);

		// Without the asynchronous flag, VideoToolbox calls `decoded` before
		// it returns.
		*self.output = Output { status: 0, image: std::ptr::null_mut() };
		let mut info = 0u32;
		let status = unsafe {
			VTDecompressionSessionDecodeFrame(self.session, sample.0 as CMSampleBufferRef, 0, std::ptr::null_mut(), &mut info)
		};
		let image = std::mem::replace(&mut self.output.image, std::ptr::null_mut());
		if status != 0 || self.output.status != 0 || image.is_null() {
			if !image.is_null() {
				unsafe { CFRelease(image) };
			}
			bail!(
				"VideoToolbox did not decode frame {}: status {status}, output status {}",
				frame.frame_index,
				self.output.status
			);
		}

		let (width, height) = unsafe { (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image)) };
		let mut texture = std::ptr::null_mut();
		let status = unsafe {
			CVMetalTextureCacheCreateTextureFromImage(
				std::ptr::null(),
				self.textures,
				image,
				std::ptr::null(),
				MTLPixelFormatBGRA8Unorm,
				width,
				height,
				0,
				&mut texture,
			)
		};
		if status != 0 || texture.is_null() {
			unsafe { CFRelease(image) };
			bail!("Core Video made no Metal texture of frame {}: status {status}", frame.frame_index);
		}
		// Releases the cache's textures that no picture holds any more.
		unsafe { CVMetalTextureCacheFlush(self.textures, 0) };
		Ok(Picture {
			texture,
			pixels: image,
			width: width as u32,
			height: height as u32,
			frame_index: frame.frame_index,
			rtp_timestamp: frame.rtp_timestamp,
			decoded_at: Instant::now(),
		})
	}

	fn close_session(&mut self) {
		if !self.session.is_null() {
			unsafe {
				VTDecompressionSessionInvalidate(self.session);
				CFRelease(self.session);
			}
			self.session = std::ptr::null_mut();
		}
	}
}

impl Drop for Decoder {
	fn drop(&mut self) {
		self.close_session();
		unsafe {
			CFRelease(self.textures);
			CFRelease(self.device);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::h264::{access_units, is_key_frame};
	use sandom_gamestream_protocol::video::FrameType;

	/// Red, green, blue and white, three 320 × 192 frames each, made by
	/// `ffmpeg -f lavfi -i color=c=0xff0000:s=320x192:r=30:d=0.0667` (and
	/// the three other colours), concatenated, converted with the BT.709
	/// limited-range matrix and encoded by libx264 (constrained baseline, no
	/// B-frames, a key frame every 8 frames, access unit delimiters).
	const COLORS: &[u8] = include_bytes!("../tests/fixtures/colors.h264");

	#[test]
	fn every_frame_decodes_to_its_colour_in_a_metal_texture() {
		let units = access_units(COLORS);
		assert_eq!(units.len(), 12);
		let mut decoder = Decoder::new(std::ptr::null_mut()).unwrap();
		// BGRA of red, green, blue and white; BT.709 limited range rounds.
		let expected = [[0, 0, 255], [0, 255, 0], [255, 0, 0], [255, 255, 255]];
		for (index, unit) in units.iter().enumerate() {
			let frame = Frame {
				frame_index: index as u32 + 1,
				frame_type: if is_key_frame(unit) { FrameType::Key } else { FrameType::Predicted },
				rtp_timestamp: Some(index as u32 * 3000),
				data: unit.to_vec(),
			};
			let picture = decoder.decode(&frame).unwrap();
			assert_eq!((picture.width, picture.height), (320, 192));
			assert!(!picture.metal_texture().is_null());
			let bytes = picture.read_bgra().unwrap();
			let center = (96 * 320 + 160) * 4;
			let pixel = &bytes[center..center + 3];
			let colour = expected[index / 3];
			assert!(
				pixel.iter().zip(colour).all(|(&got, want)| (got as i32 - want).abs() <= 12),
				"frame {index}: BGR {pixel:?}, expected {colour:?}"
			);
		}
	}

	#[test]
	fn a_predicted_frame_before_any_key_frame_is_refused() {
		let units = access_units(COLORS);
		let mut decoder = Decoder::new(std::ptr::null_mut()).unwrap();
		let frame = Frame { frame_index: 1, frame_type: FrameType::Predicted, rtp_timestamp: None, data: units[1].to_vec() };
		let error = decoder.decode(&frame).err().expect("a predicted frame without parameter sets decodes nothing");
		assert!(error.to_string().contains("before any key frame"), "{error}");
	}
}
