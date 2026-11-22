//! The Core Foundation, Core Media, Core Video and VideoToolbox functions of
//! the macOS decoder (`decoder.rs`), declared from Apple's C headers of the
//! macOS SDK.

#![allow(non_upper_case_globals, non_snake_case, non_camel_case_types)]

use std::ffi::c_void;

pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFIndex = isize;
pub type OSStatus = i32;

pub type CVPixelBufferRef = *mut c_void;
pub type CVMetalTextureCacheRef = *mut c_void;
pub type CVMetalTextureRef = *mut c_void;
pub type CMSampleBufferRef = *mut c_void;
pub type CMFormatDescriptionRef = *mut c_void;
pub type CMBlockBufferRef = *mut c_void;
pub type VTDecompressionSessionRef = *mut c_void;
/// An `id<MTLDevice>` or `id<MTLTexture>`.
pub type MTLObject = *mut c_void;

/// An opaque callback table of Core Foundation; only its address is used.
#[repr(C)]
pub struct CFCallBacks {
	_opaque: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CMTime {
	pub value: i64,
	pub timescale: i32,
	pub flags: u32,
	pub epoch: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CMSampleTimingInfo {
	pub duration: CMTime,
	pub presentation_time_stamp: CMTime,
	pub decode_time_stamp: CMTime,
}

/// kCMTimeInvalid: every field zero.
pub const CMTIME_INVALID: CMTime = CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 };
/// kCMTimeFlags_Valid.
pub const CMTIME_FLAGS_VALID: u32 = 1;

pub type VTDecompressionOutputCallback = extern "C" fn(
	output: *mut c_void,
	frame: *mut c_void,
	status: OSStatus,
	flags: u32,
	image: CVPixelBufferRef,
	presentation: CMTime,
	duration: CMTime,
);

#[repr(C)]
pub struct VTDecompressionOutputCallbackRecord {
	pub callback: VTDecompressionOutputCallback,
	pub context: *mut c_void,
}

pub const kCFNumberSInt32Type: CFIndex = 3;
pub const kCVPixelFormatType_32BGRA: u32 = u32::from_be_bytes(*b"BGRA");
/// MTLPixelFormatBGRA8Unorm.
pub const MTLPixelFormatBGRA8Unorm: usize = 80;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
	pub static kCFTypeDictionaryKeyCallBacks: CFCallBacks;
	pub static kCFTypeDictionaryValueCallBacks: CFCallBacks;
	pub static kCFBooleanTrue: CFTypeRef;
	/// An allocator that frees nothing: the caller keeps the memory alive.
	pub static kCFAllocatorNull: CFAllocatorRef;
	pub fn CFDictionaryCreateMutable(
		allocator: CFAllocatorRef,
		capacity: CFIndex,
		keys: *const CFCallBacks,
		values: *const CFCallBacks,
	) -> CFMutableDictionaryRef;
	pub fn CFDictionarySetValue(dictionary: CFMutableDictionaryRef, key: CFTypeRef, value: CFTypeRef);
	pub fn CFNumberCreate(allocator: CFAllocatorRef, kind: CFIndex, value: *const c_void) -> CFTypeRef;
	pub fn CFRetain(object: CFTypeRef) -> CFTypeRef;
	pub fn CFRelease(object: CFTypeRef);
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
	pub static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
	pub static kCVPixelBufferMetalCompatibilityKey: CFStringRef;
	pub static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
	pub fn CVPixelBufferGetWidth(buffer: CVPixelBufferRef) -> usize;
	pub fn CVPixelBufferGetHeight(buffer: CVPixelBufferRef) -> usize;
	pub fn CVPixelBufferLockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
	pub fn CVPixelBufferUnlockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
	pub fn CVPixelBufferGetBaseAddress(buffer: CVPixelBufferRef) -> *mut u8;
	pub fn CVPixelBufferGetBytesPerRow(buffer: CVPixelBufferRef) -> usize;
	pub fn CVMetalTextureCacheCreate(
		allocator: CFAllocatorRef,
		cache_attributes: CFDictionaryRef,
		device: MTLObject,
		texture_attributes: CFDictionaryRef,
		cache: *mut CVMetalTextureCacheRef,
	) -> i32;
	pub fn CVMetalTextureCacheCreateTextureFromImage(
		allocator: CFAllocatorRef,
		cache: CVMetalTextureCacheRef,
		image: CVPixelBufferRef,
		texture_attributes: CFDictionaryRef,
		pixel_format: usize,
		width: usize,
		height: usize,
		plane: usize,
		texture: *mut CVMetalTextureRef,
	) -> i32;
	pub fn CVMetalTextureGetTexture(texture: CVMetalTextureRef) -> MTLObject;
	pub fn CVMetalTextureCacheFlush(cache: CVMetalTextureCacheRef, options: u64);
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
	pub fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
		allocator: CFAllocatorRef,
		parameter_set_count: usize,
		parameter_sets: *const *const u8,
		parameter_set_sizes: *const usize,
		nal_unit_header_length: i32,
		format: *mut CMFormatDescriptionRef,
	) -> OSStatus;
	pub fn CMBlockBufferCreateWithMemoryBlock(
		structure_allocator: CFAllocatorRef,
		memory: *mut c_void,
		block_length: usize,
		block_allocator: CFAllocatorRef,
		custom_source: *const c_void,
		offset: usize,
		data_length: usize,
		flags: u32,
		block: *mut CMBlockBufferRef,
	) -> OSStatus;
	pub fn CMSampleBufferCreateReady(
		allocator: CFAllocatorRef,
		data: CMBlockBufferRef,
		format: CMFormatDescriptionRef,
		sample_count: CFIndex,
		timing_count: CFIndex,
		timing: *const CMSampleTimingInfo,
		size_count: CFIndex,
		sizes: *const usize,
		sample: *mut CMSampleBufferRef,
	) -> OSStatus;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
	pub fn VTDecompressionSessionCreate(
		allocator: CFAllocatorRef,
		format: CMFormatDescriptionRef,
		decoder_specification: CFDictionaryRef,
		image_attributes: CFDictionaryRef,
		output: *const VTDecompressionOutputCallbackRecord,
		session: *mut VTDecompressionSessionRef,
	) -> OSStatus;
	pub fn VTDecompressionSessionCanAcceptFormatDescription(
		session: VTDecompressionSessionRef,
		format: CMFormatDescriptionRef,
	) -> u8;
	pub fn VTDecompressionSessionDecodeFrame(
		session: VTDecompressionSessionRef,
		sample: CMSampleBufferRef,
		flags: u32,
		frame: *mut c_void,
		info: *mut u32,
	) -> OSStatus;
	pub fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);
}

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
	pub fn MTLCreateSystemDefaultDevice() -> MTLObject;
}

/// A Core Foundation object this code owns: released when dropped.
pub struct Owned(pub CFTypeRef);

impl Owned {
	pub fn dictionary() -> anyhow::Result<Self> {
		let dictionary = unsafe {
			CFDictionaryCreateMutable(std::ptr::null(), 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)
		};
		anyhow::ensure!(!dictionary.is_null(), "Core Foundation created no dictionary");
		Ok(Self(dictionary))
	}

	pub fn number(value: i32) -> anyhow::Result<Self> {
		let number = unsafe { CFNumberCreate(std::ptr::null(), kCFNumberSInt32Type, (&value as *const i32).cast()) };
		anyhow::ensure!(!number.is_null(), "Core Foundation created no number for {value}");
		Ok(Self(number))
	}

	/// Sets `key` to `value` in this dictionary, which retains both.
	pub unsafe fn set(&self, key: CFTypeRef, value: CFTypeRef) {
		unsafe { CFDictionarySetValue(self.0 as CFMutableDictionaryRef, key, value) };
	}
}

impl Drop for Owned {
	fn drop(&mut self) {
		if !self.0.is_null() {
			unsafe { CFRelease(self.0) };
		}
	}
}
