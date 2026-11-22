//! The C interface of the client, for hosts such as a Unity plugin.
//!
//! A host opens the client's identity, pairs once with a server (the pairing
//! is saved to a file), and starts a session that decodes into Metal
//! textures of the host's device. The session rings the host's doorbell
//! from its own threads when a picture arrives or the session ends; the
//! host then reads the latest picture (`sandom_gamestream_client_picture`)
//! and the end (`sandom_gamestream_client_ended`). A picture is a handle
//! that keeps its texture and memory until the host frees it.
//!
//! Strings are UTF-8 with a length. A call that fails returns 0 or null and
//! keeps its error for `sandom_gamestream_client_last_error` on the same
//! thread.
//!
//! Input travels through typed calls, one per kind of input event, whose
//! arguments are the GameStream protocol's values: Windows virtual-key
//! codes, mouse buttons 1 to 5 (left, middle, right, side, extra), wheel
//! amounts of 120 per notch, pointer events 0 to 7 (hover, down, up, move,
//! cancel, button only, hover leave, cancel all), pen tools 0 to 2 (unknown,
//! pen, eraser) and touch coordinates in 0..1 of the stream. Each returns 1,
//! or 0 when the value is out of range or the session cannot send.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result, anyhow, bail, ensure};

use crate::decoder::Picture;
use crate::input::{
	InputEvent, Key, MouseButton, MouseMoveAbsolute, MouseMoveRelative, MouseScrollHorizontal, MouseScrollVertical, Pen,
	PenToolKind, PointerEventKind, Touch,
};
use crate::{ClientIdentity, PairedServer, Session, SessionConfig, SessionEvent, VideoOutput, pair};

thread_local! {
	static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Runs `call`; an error becomes the thread's last error and `failed`.
fn guarded<T>(failed: T, call: impl FnOnce() -> Result<T>) -> T {
	match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
		Ok(Ok(value)) => value,
		Ok(Err(error)) => {
			LAST_ERROR.with(|last| *last.borrow_mut() = format!("{error:#}"));
			failed
		},
		Err(_) => {
			LAST_ERROR.with(|last| *last.borrow_mut() = "the client panicked; see its log".to_string());
			failed
		},
	}
}

unsafe fn text<'a>(bytes: *const u8, length: usize, what: &str) -> Result<&'a str> {
	if bytes.is_null() {
		bail!("{what} is null");
	}
	std::str::from_utf8(unsafe { std::slice::from_raw_parts(bytes, length) }).with_context(|| format!("{what} is not UTF-8"))
}

/// Copies `text` into `buffer` and returns its whole length.
unsafe fn copy_out(text: &str, buffer: *mut u8, capacity: usize) -> usize {
	if !buffer.is_null() {
		let length = text.len().min(capacity);
		unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), buffer, length) };
	}
	text.len()
}

/// The last error of this thread's last failed call, as UTF-8 in `buffer`;
/// returns its whole length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_last_error(buffer: *mut u8, capacity: usize) -> usize {
	LAST_ERROR.with(|last| unsafe { copy_out(&last.borrow(), buffer, capacity) })
}

/// Opens the client identity in `directory`, creating it there when it does
/// not exist. Free it with `sandom_gamestream_client_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_open(directory: *const u8, length: usize) -> *mut ClientIdentity {
	guarded(std::ptr::null_mut(), || {
		let directory = PathBuf::from(unsafe { text(directory, length, "the identity directory") }?);
		Ok(Box::into_raw(Box::new(ClientIdentity::load_or_create(&directory)?)))
	})
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_free(client: *mut ClientIdentity) {
	if !client.is_null() {
		drop(unsafe { Box::from_raw(client) });
	}
}

/// Pairs with the server at `address`; the user enters `pin` on the device,
/// and the call waits for that. Saves the pairing at `pairing_path`.
/// Returns 1, or 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_pair(
	client: *const ClientIdentity,
	address: *const u8,
	address_length: usize,
	http_port: u16,
	pin: *const u8,
	pin_length: usize,
	pairing_path: *const u8,
	pairing_path_length: usize,
) -> i32 {
	guarded(0, || {
		let client = unsafe { client.as_ref() }.ok_or_else(|| anyhow!("the client is null"))?;
		let address = unsafe { text(address, address_length, "the server address") }?;
		let pin = unsafe { text(pin, pin_length, "the PIN") }?;
		let path = PathBuf::from(unsafe { text(pairing_path, pairing_path_length, "the pairing path") }?);
		pair(client, address, http_port, pin)?.save(&path)?;
		Ok(1)
	})
}

/// Rung from the session's threads when a picture arrives or the session
/// ends. It must not block.
pub type Doorbell = extern "C" fn(context: *mut c_void);

/// A running session of the C interface.
pub struct NativeSession {
	session: Session,
	ended: Arc<Mutex<Option<String>>>,
}

struct Ring {
	doorbell: Doorbell,
	context: usize,
}

impl Ring {
	fn ring(&self) {
		(self.doorbell)(self.context as *mut c_void);
	}
}

/// Starts a session with the server of the pairing at `pairing_path`,
/// decoding into textures of `metal_device` (an `id<MTLDevice>`; null for
/// the system's default device). Free it with
/// `sandom_gamestream_client_end`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_start(
	client: *const ClientIdentity,
	pairing_path: *const u8,
	pairing_path_length: usize,
	width: u32,
	height: u32,
	fps: u32,
	bitrate_kbps: u32,
	metal_device: *mut c_void,
	doorbell: Doorbell,
	context: *mut c_void,
) -> *mut NativeSession {
	guarded(std::ptr::null_mut(), || {
		let client = unsafe { client.as_ref() }.ok_or_else(|| anyhow!("the client is null"))?;
		let path = PathBuf::from(unsafe { text(pairing_path, pairing_path_length, "the pairing path") }?);
		let paired = PairedServer::load(&path)?;
		let config = SessionConfig {
			width,
			height,
			fps,
			bitrate_kbps,
			video_output: VideoOutput::Metal { device: metal_device as usize },
			..SessionConfig::default()
		};
		let ended = Arc::new(Mutex::new(None));
		let ring = Ring { doorbell, context: context as usize };
		let session = Session::start(client, &paired, config, {
			let ended = ended.clone();
			move |event| match event {
				SessionEvent::Picture(_) => ring.ring(),
				SessionEvent::Ended { reason } => {
					*ended.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason);
					ring.ring();
				},
				SessionEvent::DecodeFailed { frame_index, reason } => {
					tracing::warn!("Frame {frame_index} did not decode: {reason}");
				},
				SessionEvent::FramesLost(lost) => tracing::debug!("Lost frames {lost:?}; asked for a key frame."),
				SessionEvent::Frame(_) => {},
			}
		})?;
		Ok(Box::into_raw(Box::new(NativeSession { session, ended })))
	})
}

/// The latest picture: a handle that keeps the texture and its memory until
/// `sandom_gamestream_client_picture_free`, with its size, its frame index
/// and its 90 kHz RTP timestamp (0 when no data shard carried it). Null, and
/// no error, before the first picture.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_picture(
	session: *const NativeSession,
	width: *mut u32,
	height: *mut u32,
	frame_index: *mut u32,
	rtp_timestamp: *mut u32,
) -> *mut Arc<Picture> {
	guarded(std::ptr::null_mut(), || {
		let session = unsafe { session.as_ref() }.ok_or_else(|| anyhow!("the session is null"))?;
		let Some(picture) = session.session.latest_picture() else {
			return Ok(std::ptr::null_mut());
		};
		unsafe {
			for (out, value) in [
				(width, picture.width),
				(height, picture.height),
				(frame_index, picture.frame_index),
				(rtp_timestamp, picture.rtp_timestamp.unwrap_or(0)),
			] {
				if !out.is_null() {
					*out = value;
				}
			}
		}
		Ok(Box::into_raw(Box::new(picture)))
	})
}

/// The picture's `id<MTLTexture>` (BGRA8Unorm), valid until the picture is
/// freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_picture_texture(picture: *const Arc<Picture>) -> *mut c_void {
	unsafe { picture.as_ref() }.map_or(std::ptr::null_mut(), |picture| picture.metal_texture())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_picture_free(picture: *mut Arc<Picture>) {
	if !picture.is_null() {
		drop(unsafe { Box::from_raw(picture) });
	}
}

/// Why the session ended, as UTF-8 in `buffer`; returns its whole length,
/// or -1 while the session runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_ended(session: *const NativeSession, buffer: *mut u8, capacity: usize) -> i64 {
	let Some(session) = (unsafe { session.as_ref() }) else {
		return -1;
	};
	match &*session.ended.lock().unwrap_or_else(PoisonError::into_inner) {
		Some(reason) => unsafe { copy_out(reason, buffer, capacity) as i64 },
		None => -1,
	}
}

/// Sends the input event that `event` builds; 1, or 0 with the reason.
unsafe fn send_input(session: *const NativeSession, event: impl FnOnce() -> Result<InputEvent>) -> i32 {
	guarded(0, || {
		let session = unsafe { session.as_ref() }.ok_or_else(|| anyhow!("the session is null"))?;
		session.session.send_input(event()?)?;
		Ok(1)
	})
}

fn key(virtual_key: u8) -> Result<Key> {
	Key::from_repr(virtual_key).ok_or_else(|| anyhow!("virtual-key code 0x{virtual_key:02X} is not a supported key"))
}

fn pointer_event(event: u8) -> Result<PointerEventKind> {
	PointerEventKind::from_repr(event).ok_or_else(|| anyhow!("pointer event {event} is not 0 to 7"))
}

/// Presses (`down` 1) or releases (0) the key of a Windows virtual-key code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_key(session: *const NativeSession, down: i32, virtual_key: u8) -> i32 {
	unsafe { send_input(session, || Ok(if down != 0 { InputEvent::KeyDown(key(virtual_key)?) } else { InputEvent::KeyUp(key(virtual_key)?) })) }
}

/// Moves the pointer to `x`, `y` of a screen `width` by `height`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_mouse_move(session: *const NativeSession, x: i16, y: i16, width: i16,
	height: i16) -> i32 {
	unsafe { send_input(session, || Ok(InputEvent::MouseMoveAbsolute(MouseMoveAbsolute { x, y, screen_width: width, screen_height: height }))) }
}

/// Moves the pointer by `x`, `y`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_mouse_move_relative(session: *const NativeSession, x: i16, y: i16) -> i32 {
	unsafe { send_input(session, || Ok(InputEvent::MouseMoveRelative(MouseMoveRelative { x, y }))) }
}

/// Presses (`down` 1) or releases (0) mouse button 1 to 5.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_mouse_button(session: *const NativeSession, down: i32, button: u8) -> i32 {
	unsafe {
		send_input(session, || {
			let button = MouseButton::from_repr(button).ok_or_else(|| anyhow!("mouse button {button} is not 1 to 5"))?;
			Ok(if down != 0 { InputEvent::MouseButtonDown(button) } else { InputEvent::MouseButtonUp(button) })
		})
	}
}

/// Turns the wheel by `amount` (120 per notch), vertically (`horizontal` 0)
/// or horizontally (1).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_scroll(session: *const NativeSession, horizontal: i32, amount: i16) -> i32 {
	unsafe {
		send_input(session, || {
			Ok(if horizontal != 0 {
				InputEvent::MouseScrollHorizontal(MouseScrollHorizontal { amount })
			} else {
				InputEvent::MouseScrollVertical(MouseScrollVertical { amount })
			})
		})
	}
}

/// Sends pointer event `event` of touch contact `pointer_id` at `x`, `y`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_touch(session: *const NativeSession, event: u8, pointer_id: u32, x: f32,
	y: f32) -> i32 {
	unsafe { send_input(session, || Ok(InputEvent::Touch(Touch { event_kind: pointer_event(event)?, pointer_id, x, y }))) }
}

/// Sends pointer event `event` of a pen.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_pen(session: *const NativeSession, event: u8, tool: u8, buttons: u8, x: f32,
	y: f32, pressure_or_distance: f32, rotation: u16, tilt: u8) -> i32 {
	unsafe {
		send_input(session, || {
			let tool_kind = PenToolKind::from_repr(tool).ok_or_else(|| anyhow!("pen tool {tool} is not 0 to 2"))?;
			Ok(InputEvent::Pen(Pen { event_kind: pointer_event(event)?, tool_kind, buttons, x, y, pressure_or_distance, rotation, tilt }))
		})
	}
}

/// Types the UTF-8 text of `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_text(session: *const NativeSession, text: *const u8, length: usize) -> i32 {
	unsafe {
		send_input(session, || {
			let text = self::text(text, length, "the text")?;
			ensure!(!text.is_empty(), "the text is empty");
			Ok(InputEvent::Utf8Text(text.to_string()))
		})
	}
}

/// Ends the session on the server (`/cancel`) and frees it. Returns 1, or 0
/// when `/cancel` failed; the session is freed either way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sandom_gamestream_client_end(session: *mut NativeSession) -> i32 {
	guarded(0, || {
		if session.is_null() {
			bail!("the session is null");
		}
		let session = unsafe { Box::from_raw(session) };
		session.session.end()?;
		Ok(1)
	})
}
