//! The C interface of the server, which Deno's FFI and the Node-API addon
//! (node/) call. A program starts a server with its configuration as JSON
//! and a doorbell: the server rings the doorbell from its own threads when
//! its event queue turns non-empty, and the program then takes every event
//! as a JSON record with `sandom_gamestream_take_event` until none is left.
//!
//! Event records:
//!
//! - `{"kind":"pinRequired","client":<uniqueid>}`
//! - `{"kind":"sessionStarted","width","height","fps","codec":"h264","bitrate","maxReferenceFrames","fullRange"}`
//! - `{"kind":"keyFrameRequested"}`
//! - `{"kind":"input","input":<input record>}`
//! - `{"kind":"sessionEnded"}`
//!
//! Input records, by `type`: `keyDown` and `keyUp` (`key`, the Windows
//! virtual-key code, and `name`); `mouseMoveAbsolute` (`x`, `y`,
//! `screenWidth`, `screenHeight`); `mouseMoveRelative` (`x`, `y`);
//! `mouseButtonDown` and `mouseButtonUp` (`button`: `left`, `middle`,
//! `right`, `side` or `extra`); `scrollVertical` and `scrollHorizontal`
//! (`amount`, 120 per notch); `touch` (`event`, `pointerId`, `x`, `y` in
//! 0..1 of the stream); `pen` (`event`, `tool`, `buttons`, `x`, `y`,
//! `pressureOrDistance`, `rotation`, `tilt`); `text` (`text`). A pointer
//! `event` is `hover`, `down`, `up`, `move`, `cancel`, `buttonOnly`,
//! `hoverLeave` or `cancelAll`; a pen `tool` is `unknown`, `pen` or
//! `eraser`.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
	Codec, InputEvent, MouseButton, PenToolKind, PointerEventKind, Server, ServerConfig, ServerEvent, ServerPorts,
};

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

/// The configuration record of `sandom_gamestream_start`; a port left out
/// is the GameStream default.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartRecord {
	host_name: String,
	certificate_directory: PathBuf,
	application_title: String,
	bind_address: String,
	#[serde(default)]
	ports: PortsRecord,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortsRecord {
	http: Option<u16>,
	https: Option<u16>,
	rtsp: Option<u16>,
	video: Option<u16>,
	control: Option<u16>,
	audio: Option<u16>,
}

struct Ring {
	doorbell: Doorbell,
	context: usize,
}

struct Queue {
	events: Mutex<VecDeque<Vec<u8>>>,
	ring: Ring,
}

impl Queue {
	fn push(&self, record: Value) {
		let bytes = serde_json::to_vec(&record).expect("an event record serializes");
		let was_empty = {
			let mut events = self.events.lock();
			let was_empty = events.is_empty();
			events.push_back(bytes);
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

fn pointer_event(kind: PointerEventKind) -> &'static str {
	match kind {
		PointerEventKind::Hover => "hover",
		PointerEventKind::Down => "down",
		PointerEventKind::Up => "up",
		PointerEventKind::Move => "move",
		PointerEventKind::Cancel => "cancel",
		PointerEventKind::ButtonOnly => "buttonOnly",
		PointerEventKind::HoverLeave => "hoverLeave",
		PointerEventKind::CancelAll => "cancelAll",
	}
}

fn mouse_button(button: MouseButton) -> &'static str {
	match button {
		MouseButton::Left => "left",
		MouseButton::Middle => "middle",
		MouseButton::Right => "right",
		MouseButton::Side => "side",
		MouseButton::Extra => "extra",
	}
}

fn input_record(input: InputEvent) -> Value {
	match input {
		InputEvent::KeyDown(key) => json!({"type": "keyDown", "key": key as u8, "name": format!("{key:?}")}),
		InputEvent::KeyUp(key) => json!({"type": "keyUp", "key": key as u8, "name": format!("{key:?}")}),
		InputEvent::MouseMoveAbsolute(event) => json!({"type": "mouseMoveAbsolute", "x": event.x, "y": event.y,
			"screenWidth": event.screen_width, "screenHeight": event.screen_height}),
		InputEvent::MouseMoveRelative(event) => json!({"type": "mouseMoveRelative", "x": event.x, "y": event.y}),
		InputEvent::MouseButtonDown(button) => json!({"type": "mouseButtonDown", "button": mouse_button(button)}),
		InputEvent::MouseButtonUp(button) => json!({"type": "mouseButtonUp", "button": mouse_button(button)}),
		InputEvent::MouseScrollVertical(event) => json!({"type": "scrollVertical", "amount": event.amount}),
		InputEvent::MouseScrollHorizontal(event) => json!({"type": "scrollHorizontal", "amount": event.amount}),
		InputEvent::Touch(touch) => json!({"type": "touch", "event": pointer_event(touch.event_kind),
			"pointerId": touch.pointer_id, "x": touch.x, "y": touch.y}),
		InputEvent::Pen(pen) => json!({"type": "pen", "event": pointer_event(pen.event_kind),
			"tool": match pen.tool_kind { PenToolKind::Unknown => "unknown", PenToolKind::Pen => "pen", PenToolKind::Eraser => "eraser" },
			"buttons": pen.buttons, "x": pen.x, "y": pen.y, "pressureOrDistance": pen.pressure_or_distance,
			"rotation": pen.rotation, "tilt": pen.tilt}),
		InputEvent::Utf8Text(text) => json!({"type": "text", "text": text}),
	}
}

fn event_record(event: ServerEvent) -> Value {
	match event {
		ServerEvent::PinRequired { client } => json!({"kind": "pinRequired", "client": client}),
		ServerEvent::SessionStarted { width, height, fps, codec, bitrate, max_reference_frames, full_range } => json!({
			"kind": "sessionStarted", "width": width, "height": height, "fps": fps,
			"codec": match codec { Codec::H264 => "h264" }, "bitrate": bitrate,
			"maxReferenceFrames": max_reference_frames, "fullRange": full_range,
		}),
		ServerEvent::KeyFrameRequested => json!({"kind": "keyFrameRequested"}),
		ServerEvent::Input(input) => json!({"kind": "input", "input": input_record(input)}),
		ServerEvent::SessionEnded => json!({"kind": "sessionEnded"}),
	}
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
	let record: StartRecord = serde_json::from_slice(config)
		.map_err(|error| anyhow::anyhow!("the server configuration is no valid record: {error}"))?;
	let defaults = ServerPorts::default();
	let ports = ServerPorts {
		http: record.ports.http.unwrap_or(defaults.http),
		https: record.ports.https.unwrap_or(defaults.https),
		rtsp: record.ports.rtsp.unwrap_or(defaults.rtsp),
		video: record.ports.video.unwrap_or(defaults.video),
		control: record.ports.control.unwrap_or(defaults.control),
		audio: record.ports.audio.unwrap_or(defaults.audio),
	};
	let queue = Arc::new(Queue {
		events: Mutex::new(VecDeque::new()),
		ring: Ring { doorbell, context: context as usize },
	});
	let sink = Arc::clone(&queue);
	let server = Server::start(
		ServerConfig {
			host_name: record.host_name,
			certificate_directory: record.certificate_directory,
			application_title: record.application_title,
			bind_address: record.bind_address,
			ports,
		},
		move |event| sink.push(event_record(event)),
	)?;
	Ok(NativeServer { server: Some(server), queue, last_error: Mutex::new(String::new()) })
}

/// Starts a server from the JSON configuration `config`: `hostName`,
/// `certificateDirectory`, `applicationTitle`, `bindAddress` and optional
/// `ports` (`http`, `https`, `rtsp`, `video`, `control`, `audio`). Returns
/// null with the reason in `error` when the server does not start.
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
