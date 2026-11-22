//! The C interface of the server, which Deno's FFI and the Node-API addon
//! (node/) call. A program starts a server with its binary configuration and
//! a doorbell: the server rings the doorbell from its own threads when its
//! event queue turns non-empty, and the program then takes every event
//! record with `sandom_gamestream_take_event` until none is left. Numbers
//! are little-endian; a string is its byte length (u32) and its UTF-8.
//!
//! Configuration: version (u8, 1); the ports `http`, `https`, `rtsp`,
//! `video`, `control` and `audio` (u16 each, 0 for the GameStream default);
//! then the strings host name, certificate directory, application title and
//! bind address.
//!
//! Event records, by their first byte:
//!
//! - 1, pin required: the client's unique ID (string).
//! - 2, session started: width, height and fps (u32 each), codec (u8, 1 for
//!   H.264), bit rate in bits per second (u64), maximum reference frames
//!   (u32), full range (u8, 0 or 1).
//! - 3, key frame requested.
//! - 4, input: the input event in the GameStream protocol's binary format
//!   (`sandom_gamestream_protocol::input`), to the record's end.
//! - 5, session ended.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, ensure};

use crate::{Codec, Server, ServerConfig, ServerEvent, ServerPorts};

/// The version of the configuration record.
const CONFIGURATION_VERSION: u8 = 1;

pub const EVENT_PIN_REQUIRED: u8 = 1;
pub const EVENT_SESSION_STARTED: u8 = 2;
pub const EVENT_KEY_FRAME_REQUESTED: u8 = 3;
pub const EVENT_INPUT: u8 = 4;
pub const EVENT_SESSION_ENDED: u8 = 5;
/// The codec byte of a session-started record.
pub const CODEC_H264: u8 = 1;

/// `std::sync::Mutex`, whose holders never panic: a poisoned lock is a
/// defect of this module.
struct Mutex<T>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
	fn new(value: T) -> Self {
		Self(std::sync::Mutex::new(value))
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, T> {
		self.0.lock().expect("a holder of the server's lock panicked")
	}
}

/// Rung from a server thread when the event queue turns non-empty.
pub type Doorbell = extern "C" fn(context: *mut c_void);

struct Ring {
	doorbell: Doorbell,
	context: usize,
}

struct Queue {
	events: Mutex<VecDeque<Vec<u8>>>,
	ring: Ring,
}

impl Queue {
	fn push(&self, record: Vec<u8>) {
		let was_empty = {
			let mut events = self.events.lock();
			let was_empty = events.is_empty();
			events.push_back(record);
			was_empty
		};
		if was_empty {
			(self.ring.doorbell)(self.ring.context as *mut c_void);
		}
	}
}

/// A running server and its event queue, which the C interface hands out
/// as an opaque pointer.
pub struct NativeServer {
	server: Option<Server>,
	queue: Arc<Queue>,
	last_error: Mutex<String>,
}

fn push_string(record: &mut Vec<u8>, text: &str) {
	record.extend((text.len() as u32).to_le_bytes());
	record.extend(text.as_bytes());
}

/// The binary record of `event` (see the module documentation).
pub fn event_record(event: ServerEvent) -> Vec<u8> {
	match event {
		ServerEvent::PinRequired { client } => {
			let mut record = vec![EVENT_PIN_REQUIRED];
			push_string(&mut record, &client);
			record
		},
		ServerEvent::SessionStarted { width, height, fps, codec, bitrate, max_reference_frames, full_range } => {
			let mut record = vec![EVENT_SESSION_STARTED];
			record.extend(width.to_le_bytes());
			record.extend(height.to_le_bytes());
			record.extend(fps.to_le_bytes());
			record.push(match codec {
				Codec::H264 => CODEC_H264,
			});
			record.extend((bitrate as u64).to_le_bytes());
			record.extend(max_reference_frames.to_le_bytes());
			record.push(u8::from(full_range));
			record
		},
		ServerEvent::KeyFrameRequested => vec![EVENT_KEY_FRAME_REQUESTED],
		ServerEvent::Input(input) => {
			let mut record = vec![EVENT_INPUT];
			record.extend(input.to_bytes());
			record
		},
		ServerEvent::SessionEnded => vec![EVENT_SESSION_ENDED],
	}
}

/// Reads the fields of a configuration record in order.
struct ConfigurationReader<'a> {
	rest: &'a [u8],
}

impl<'a> ConfigurationReader<'a> {
	fn take(&mut self, count: usize, what: &str) -> anyhow::Result<&'a [u8]> {
		ensure!(self.rest.len() >= count, "the configuration ends before its {what}");
		let (taken, rest) = self.rest.split_at(count);
		self.rest = rest;
		Ok(taken)
	}

	fn port(&mut self, what: &str, default: u16) -> anyhow::Result<u16> {
		let port = u16::from_le_bytes(self.take(2, what)?.try_into().expect("two bytes"));
		Ok(if port == 0 { default } else { port })
	}

	fn string(&mut self, what: &str) -> anyhow::Result<String> {
		let length = u32::from_le_bytes(self.take(4, what)?.try_into().expect("four bytes")) as usize;
		let bytes = self.take(length, what)?;
		String::from_utf8(bytes.to_vec()).with_context(|| format!("the configuration's {what} is not UTF-8"))
	}
}

/// The server configuration in `bytes` (see the module documentation).
fn read_configuration(bytes: &[u8]) -> anyhow::Result<ServerConfig> {
	let mut reader = ConfigurationReader { rest: bytes };
	let version = reader.take(1, "version")?[0];
	ensure!(version == CONFIGURATION_VERSION, "configuration version {version} is not {CONFIGURATION_VERSION}");
	let defaults = ServerPorts::default();
	let ports = ServerPorts {
		http: reader.port("HTTP port", defaults.http)?,
		https: reader.port("HTTPS port", defaults.https)?,
		rtsp: reader.port("RTSP port", defaults.rtsp)?,
		video: reader.port("video port", defaults.video)?,
		control: reader.port("control port", defaults.control)?,
		audio: reader.port("audio port", defaults.audio)?,
	};
	let host_name = reader.string("host name")?;
	let certificate_directory = PathBuf::from(reader.string("certificate directory")?);
	let application_title = reader.string("application title")?;
	let bind_address = reader.string("bind address")?;
	ensure!(reader.rest.is_empty(), "the configuration has {} bytes after its last field", reader.rest.len());
	Ok(ServerConfig { host_name, certificate_directory, application_title, bind_address, ports })
}

/// Writes `message` into the caller's buffer, as much as fits, and its full
/// length into `length`.
unsafe fn write_message(message: &str, buffer: *mut u8, capacity: usize, length: *mut usize) {
	let bytes = message.as_bytes();
	let count = bytes.len().min(capacity);
	if count > 0 && !buffer.is_null() {
		unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, count) };
	}
	if !length.is_null() {
		unsafe { *length = bytes.len() };
	}
}

fn start(config: &[u8], doorbell: Doorbell, context: *mut c_void) -> anyhow::Result<NativeServer> {
	let config = read_configuration(config)?;
	let queue = Arc::new(Queue {
		events: Mutex::new(VecDeque::new()),
		ring: Ring { doorbell, context: context as usize },
	});
	let sink = Arc::clone(&queue);
	let server = Server::start(
		config,
		move |event| sink.push(event_record(event)),
	)?;
	Ok(NativeServer { server: Some(server), queue, last_error: Mutex::new(String::new()) })
}

/// Starts a server from the binary configuration `config` (see the module
/// documentation). Returns null with the reason in `error` when the server
/// does not start.
#[unsafe(no_mangle)]
pub extern "C" fn sandom_gamestream_start(config: *const u8, config_length: usize, doorbell: Option<Doorbell>,
	context: *mut c_void, error: *mut u8, capacity: usize, error_length: *mut usize) -> *mut NativeServer {
	let result = match doorbell {
		None => Err(anyhow::anyhow!("the server takes a doorbell function")),
		Some(doorbell) => start(unsafe { std::slice::from_raw_parts(config, config_length) }, doorbell, context),
	};
	match result {
		Ok(server) => Box::into_raw(Box::new(server)),
		Err(reason) => {
			unsafe { write_message(&format!("{reason:#}"), error, capacity, error_length) };
			std::ptr::null_mut()
		}
	}
}

/// Takes the next event record: writes it into `buffer` and returns its
/// length, or returns 0 when the queue is empty. A record longer than
/// `capacity` stays in the queue, and the call returns its length negated.
#[unsafe(no_mangle)]
pub extern "C" fn sandom_gamestream_take_event(server: *const NativeServer, buffer: *mut u8, capacity: usize) -> i64 {
	let server = unsafe { &*server };
	let mut events = server.queue.events.lock();
	let Some(record) = events.front() else { return 0 };
	if record.len() > capacity {
		return -(record.len() as i64);
	}
	let record = events.pop_front().expect("the queue holds the record it showed");
	unsafe { std::ptr::copy_nonoverlapping(record.as_ptr(), buffer, record.len()) };
	record.len() as i64
}

fn running(server: &NativeServer) -> anyhow::Result<&Server> {
	server.server.as_ref().ok_or_else(|| anyhow::anyhow!("the server has stopped"))
}

/// Records the failure of a call for `sandom_gamestream_last_error`: 0 on
/// success, -1 on failure.
fn status(server: &NativeServer, result: anyhow::Result<()>) -> i32 {
	match result {
		Ok(()) => 0,
		Err(reason) => {
			*server.last_error.lock() = format!("{reason:#}");
			-1
		}
	}
}

/// Sends one H.264 access unit (Annex B) of the running session; `key` is 1
/// for a key frame. Returns 0, or -1 with the reason in the last error.
#[unsafe(no_mangle)]
pub extern "C" fn sandom_gamestream_send_access_unit(server: *const NativeServer, data: *const u8, length: usize, key: u32,
	timestamp_us: i64) -> i32 {
	let server = unsafe { &*server };
	let data = unsafe { std::slice::from_raw_parts(data, length) };
	status(server, running(server).and_then(|running| running.send_access_unit(data, key != 0, timestamp_us)))
}

/// Ends the running session; `sessionEnded` follows. It waits for the
/// session's tasks, which may wait for the doorbell, so a caller whose
/// doorbell runs on the calling thread calls it from another thread.
#[unsafe(no_mangle)]
pub extern "C" fn sandom_gamestream_end_session(server: *const NativeServer) -> i32 {
	let server = unsafe { &*server };
	status(server, running(server).and_then(Server::end_session))
}

/// Stops every task of the server and closes its ports; the doorbell rings
/// no more after it returns. The server's events stay to be taken until
/// `sandom_gamestream_free`. It waits as `sandom_gamestream_end_session`
/// does.
#[unsafe(no_mangle)]
pub extern "C" fn sandom_gamestream_stop(server: *mut NativeServer) -> i32 {
	let server = unsafe { &mut *server };
	let result = server.server.take().ok_or_else(|| anyhow::anyhow!("the server has stopped")).and_then(Server::stop);
	status(server, result)
}

/// Frees a server that `sandom_gamestream_stop` stopped.
#[unsafe(no_mangle)]
pub extern "C" fn sandom_gamestream_free(server: *mut NativeServer) -> i32 {
	if unsafe { &*server }.server.is_some() {
		let server = unsafe { &*server };
		*server.last_error.lock() = "a running server is not freed; stop it first".to_string();
		return -1;
	}
	drop(unsafe { Box::from_raw(server) });
	0
}

/// Writes the reason of the server's last failed call into `buffer`, as
/// much as fits, and returns its full length.
#[unsafe(no_mangle)]
pub extern "C" fn sandom_gamestream_last_error(server: *const NativeServer, buffer: *mut u8, capacity: usize) -> usize {
	let server = unsafe { &*server };
	let mut length = 0;
	unsafe { write_message(&server.last_error.lock(), buffer, capacity, &mut length) };
	length
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		InputEvent, Key, MouseButton, MouseMoveAbsolute, MouseMoveRelative, MouseScrollHorizontal, MouseScrollVertical, Pen,
		PenToolKind, PointerEventKind, Touch,
	};

	/// bindings/server.test.js decodes the same fixture into the events'
	/// JavaScript records.
	#[test]
	fn event_records_match_the_binding_fixture() {
		let events = [
			ServerEvent::PinRequired { client: "C1".to_string() },
			ServerEvent::SessionStarted {
				width: 1280,
				height: 720,
				fps: 60,
				codec: Codec::H264,
				bitrate: 10_000_000,
				max_reference_frames: 1,
				full_range: false,
			},
			ServerEvent::KeyFrameRequested,
			ServerEvent::Input(InputEvent::KeyDown(Key::A)),
			ServerEvent::Input(InputEvent::MouseMoveAbsolute(MouseMoveAbsolute { x: -5, y: 96, screen_width: 1280, screen_height: 720 })),
			ServerEvent::Input(InputEvent::MouseMoveRelative(MouseMoveRelative { x: -3, y: 4 })),
			ServerEvent::Input(InputEvent::MouseButtonUp(MouseButton::Right)),
			ServerEvent::Input(InputEvent::MouseScrollVertical(MouseScrollVertical { amount: -120 })),
			ServerEvent::Input(InputEvent::MouseScrollHorizontal(MouseScrollHorizontal { amount: 240 })),
			ServerEvent::Input(InputEvent::Touch(Touch { event_kind: PointerEventKind::Down, pointer_id: 2, x: 0.5, y: 0.25 })),
			ServerEvent::Input(InputEvent::Pen(Pen {
				event_kind: PointerEventKind::Move,
				tool_kind: PenToolKind::Eraser,
				buttons: 1,
				x: 0.5,
				y: 0.75,
				pressure_or_distance: 0.25,
				rotation: 90,
				tilt: 30,
			})),
			ServerEvent::Input(InputEvent::Utf8Text("hé".to_string())),
			ServerEvent::SessionEnded,
		];
		let encoded: Vec<String> = events
			.into_iter()
			.map(|event| event_record(event).iter().map(|byte| format!("{byte:02x}")).collect())
			.collect();
		let fixture: Vec<&str> = include_str!("../bindings/event-records.hex").lines().collect();
		assert_eq!(encoded, fixture);
	}

	#[test]
	fn a_configuration_record_names_its_fault() {
		let error = read_configuration(&[2]).unwrap_err();
		assert_eq!(error.to_string(), "configuration version 2 is not 1");
		let error = read_configuration(&[1, 0, 0]).unwrap_err();
		assert_eq!(error.to_string(), "the configuration ends before its HTTPS port");
	}
}
