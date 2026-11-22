//! The client against the server, in one process over loopback: pairing,
//! a session, video frames and input.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use sandom_gamestream::{Server, ServerConfig, ServerEvent, ServerPorts};
use sandom_gamestream_client::input::{
	InputEvent, Key, MouseButton, MouseMoveAbsolute, MouseScrollVertical, PointerEventKind, Touch,
};
use sandom_gamestream_client::{ClientIdentity, Session, SessionConfig, SessionEvent, pair};

const PIN: &str = "4719";
const WAIT: Duration = Duration::from_secs(20);

fn free_port() -> u16 {
	TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The user enters the PIN on the device: `POST /submit-pin`.
fn submit_pin(http_port: u16, client: &str) {
	let body = format!("uniqueid={client}&pin={PIN}");
	let mut connection = TcpStream::connect(("127.0.0.1", http_port)).unwrap();
	write!(
		connection,
		"POST /submit-pin HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
		body.len()
	)
	.unwrap();
	let mut response = String::new();
	connection.read_to_string(&mut response).unwrap();
	assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}

/// The next value of `receiver` that `select` takes, within `WAIT`.
fn wait_for<T, U>(receiver: &Receiver<T>, what: &str, mut select: impl FnMut(T) -> Option<U>) -> U {
	let deadline = Instant::now() + WAIT;
	loop {
		let remaining = deadline.saturating_duration_since(Instant::now());
		let value = receiver.recv_timeout(remaining).unwrap_or_else(|_| panic!("no {what} within {WAIT:?}"));
		if let Some(selected) = select(value) {
			return selected;
		}
	}
}

/// A test access unit: no decoder reads it, so any bytes do.
fn access_unit(index: u32, length: usize) -> Vec<u8> {
	(0..length).map(|offset| (offset as u32).wrapping_mul(7).wrapping_add(index) as u8).collect()
}

/// A server over loopback and a client paired with it.
struct Paired {
	_directory: tempfile::TempDir,
	server: Server,
	server_rx: Receiver<ServerEvent>,
	client: ClientIdentity,
	paired: sandom_gamestream_client::PairedServer,
}

/// A server over loopback, which takes the PIN when a client pairs.
fn start_server() -> (tempfile::TempDir, Server, Receiver<ServerEvent>, ServerPorts) {
	let directory = tempfile::tempdir().unwrap();
	let ports = ServerPorts {
		http: free_port(),
		https: free_port(),
		rtsp: free_port(),
		video: free_port(),
		control: free_port(),
		audio: free_port(),
	};
	let (server_events, server_rx) = channel();
	let server = Server::start(
		ServerConfig {
			host_name: "sandom-loopback".to_string(),
			certificate_directory: directory.path().join("server"),
			application_title: "Sandom".to_string(),
			bind_address: "127.0.0.1".to_string(),
			ports,
		},
		move |event| {
			if let ServerEvent::PinRequired { client } = &event {
				let client = client.clone();
				std::thread::spawn(move || submit_pin(ports.http, &client));
			}
			let _ = server_events.send(event);
		},
	)
	.unwrap();
	(directory, server, server_rx, ports)
}

fn start_and_pair() -> Paired {
	let (directory, server, server_rx, ports) = start_server();
	let client = ClientIdentity::load_or_create(&directory.path().join("client")).unwrap();
	let paired = pair(&client, "127.0.0.1", ports.http, PIN).unwrap();
	assert_eq!(paired.https_port, ports.https);
	Paired { _directory: directory, server, server_rx, client, paired }
}

/// A client pairs, streams and sends input. It asks for encrypted video;
/// this server does not encrypt video, so the session's video is plain.
/// The protocol crate's assembler tests cover encrypted video.
#[test]
fn a_paired_client_streams_video_and_sends_input() {
	let Paired { _directory, server, server_rx, client, paired } = start_and_pair();

	// A session.
	let (client_events, client_rx) = channel();
	let config = SessionConfig { width: 640, height: 400, fps: 30, encrypt_video: true, ..SessionConfig::default() };
	let session = Session::start(&client, &paired, config, move |event| {
		let _ = client_events.send(event);
	})
	.unwrap();
	assert!(!session.video_encrypted(), "the server advertises no video encryption");
	let (width, height) = wait_for(&server_rx, "session start", |event| match event {
		ServerEvent::SessionStarted { width, height, .. } => Some((width, height)),
		_ => None,
	});
	assert_eq!((width, height), (640, 400));

	// The server sends video once the client's PING arrives, which no event
	// reports: send key frames until the first one arrives.
	let deadline = Instant::now() + WAIT;
	let mut timestamp = 0;
	let first = loop {
		assert!(Instant::now() < deadline, "no frame reached the client within {WAIT:?}");
		timestamp += 33_333;
		server.send_access_unit(&access_unit(0, 2000), true, timestamp).unwrap();
		match client_rx.recv_timeout(Duration::from_millis(50)) {
			Ok(SessionEvent::Frame(frame)) => break frame,
			Ok(SessionEvent::Ended { reason }) => panic!("the session ended: {reason}"),
			Ok(_) | Err(_) => {},
		}
	};
	assert_eq!(first.data, access_unit(0, 2000));

	// Every access unit arrives with its bytes, its type and its timestamp;
	// the large one spans several Reed-Solomon blocks.
	for (index, length, key) in [(1, 10, false), (2, 1400, false), (3, 300_000, true), (4, 5000, false)] {
		timestamp += 33_333;
		let data = access_unit(index, length);
		server.send_access_unit(&data, key, timestamp).unwrap();
		let frame = wait_for(&client_rx, "a frame", |event| match event {
			SessionEvent::Frame(frame) => Some(frame),
			SessionEvent::Ended { reason } => panic!("the session ended: {reason}"),
			SessionEvent::FramesLost(lost) => panic!("frames {lost:?} were lost on loopback"),
			other => panic!("unexpected {other:?}"),
		});
		assert_eq!(frame.data, data, "access unit {index}");
		assert_eq!(frame.frame_type == sandom_gamestream_client::FrameType::Key, key, "access unit {index}");
		assert_eq!(frame.rtp_timestamp, Some((timestamp as u64 * 9 / 100) as u32), "access unit {index}");
	}

	// Every input event arrives as the server's input event.
	let inputs = [
		InputEvent::MouseMoveAbsolute(MouseMoveAbsolute { x: 320, y: 200, screen_width: 640, screen_height: 400 }),
		InputEvent::MouseButtonDown(MouseButton::Left),
		InputEvent::MouseButtonUp(MouseButton::Left),
		InputEvent::MouseScrollVertical(MouseScrollVertical { amount: -120 }),
		InputEvent::KeyDown(Key::A),
		InputEvent::KeyUp(Key::A),
		InputEvent::Utf8Text("héllo".to_string()),
		InputEvent::Touch(Touch { event_kind: PointerEventKind::Down, pointer_id: 1, x: 0.25, y: 0.75 }),
	];
	for input in &inputs {
		session.send_input(input.clone()).unwrap();
	}
	for input in &inputs {
		let received = wait_for(&server_rx, "an input event", |event| match event {
			ServerEvent::Input(event) => Some(event),
			_ => None,
		});
		assert_eq!(&received, input);
	}

	// Ending the session ends the server's session.
	session.end().unwrap();
	wait_for(&server_rx, "the session's end", |event| matches!(event, ServerEvent::SessionEnded).then_some(()));
	server.stop().unwrap();
}

/// The fixture of the decoder tests (`src/decoder.rs`): red, green, blue
/// and white, three 320 × 192 frames each.
#[cfg(target_os = "macos")]
const COLORS: &[u8] = include_bytes!("fixtures/colors.h264");

/// A Metal session decodes the server's H.264 frames into textures of their
/// colours, and reports how long each took from the server's send to the
/// decoded texture.
#[cfg(target_os = "macos")]
#[test]
fn a_metal_session_decodes_each_frame_into_a_texture() {
	use sandom_gamestream_client::h264::{access_units, is_key_frame};
	use sandom_gamestream_client::VideoOutput;

	let Paired { _directory, server, server_rx, client, paired } = start_and_pair();
	let (client_events, client_rx) = channel();
	let config = SessionConfig {
		width: 320,
		height: 192,
		fps: 30,
		video_output: VideoOutput::Metal { device: 0 },
		..SessionConfig::default()
	};
	let session = Session::start(&client, &paired, config, move |event| {
		let _ = client_events.send(event);
	})
	.unwrap();
	wait_for(&server_rx, "session start", |event| matches!(event, ServerEvent::SessionStarted { .. }).then_some(()));

	let units = access_units(COLORS);
	// Key frames until the client's PING has reached the server. The server
	// numbers frames from 1 in send order.
	let deadline = Instant::now() + WAIT;
	let mut timestamp = 0;
	let mut sent_frames = 0u32;
	loop {
		assert!(Instant::now() < deadline, "no picture reached the client within {WAIT:?}");
		timestamp += 33_333;
		server.send_access_unit(units[0], true, timestamp).unwrap();
		sent_frames += 1;
		match client_rx.recv_timeout(Duration::from_millis(50)) {
			Ok(SessionEvent::Picture(_)) => break,
			Ok(SessionEvent::Ended { reason }) => panic!("the session ended: {reason}"),
			Ok(SessionEvent::DecodeFailed { reason, .. }) => panic!("decoding failed: {reason}"),
			Ok(_) | Err(_) => {},
		}
	}

	let expected = [[0, 0, 255], [0, 255, 0], [255, 0, 0], [255, 255, 255]];
	let mut latencies = Vec::new();
	for (index, unit) in units.iter().enumerate().skip(1) {
		timestamp += 33_333;
		let sent = Instant::now();
		server.send_access_unit(unit, is_key_frame(unit), timestamp).unwrap();
		sent_frames += 1;
		// Pictures of the warm-up's later key frames come first.
		let picture = wait_for(&client_rx, "a picture", |event| match event {
			SessionEvent::Picture(picture) if picture.frame_index == sent_frames => Some(picture),
			SessionEvent::Picture(picture) if picture.frame_index < sent_frames => None,
			SessionEvent::Ended { reason } => panic!("the session ended: {reason}"),
			other => panic!("unexpected {other:?}"),
		});
		latencies.push(picture.decoded_at.duration_since(sent));
		assert_eq!((picture.width, picture.height), (320, 192));
		assert!(!picture.metal_texture().is_null());
		let bytes = picture.read_bgra().unwrap();
		let center = (96 * 320 + 160) * 4;
		let colour = expected[index / 3];
		assert!(
			bytes[center..center + 3].iter().zip(colour).all(|(&got, want)| (got as i32 - want).abs() <= 12),
			"frame {index}: BGR {:?}, expected {colour:?}",
			&bytes[center..center + 3]
		);
		let latest = session.latest_picture().expect("a decoded session has a latest picture");
		assert_eq!(latest.frame_index, picture.frame_index);
	}
	latencies.sort();
	eprintln!(
		"send to decoded texture over loopback, {} frames: min {:?}, median {:?}, max {:?}",
		latencies.len(),
		latencies[0],
		latencies[latencies.len() / 2],
		latencies[latencies.len() - 1]
	);
	session.end().unwrap();
	server.stop().unwrap();
}

/// The doorbell of the C-interface test: counts rings and wakes the test.
#[cfg(target_os = "macos")]
extern "C" fn ring(context: *mut std::ffi::c_void) {
	let (rings, woken) = unsafe { &*(context as *const (std::sync::Mutex<u32>, std::sync::Condvar)) };
	*rings.lock().unwrap() += 1;
	woken.notify_all();
}

/// A host drives a session through the C interface, as a Unity plugin does.
#[cfg(target_os = "macos")]
#[test]
fn a_host_pairs_streams_and_sends_input_through_the_c_interface() {
	use sandom_gamestream_client::h264::access_units;
	use sandom_gamestream_client::native::*;

	let (directory, server, server_rx, ports) = start_server();
	let identity = directory.path().join("client").to_str().unwrap().to_string();
	let pairing = directory.path().join("pairing.txt").to_str().unwrap().to_string();
	let error = || {
		let mut buffer = vec![0u8; 1024];
		let length = unsafe { sandom_gamestream_client_last_error(buffer.as_mut_ptr(), buffer.len()) };
		String::from_utf8_lossy(&buffer[..length.min(buffer.len())]).into_owned()
	};

	let client = unsafe { sandom_gamestream_client_open(identity.as_ptr(), identity.len()) };
	assert!(!client.is_null(), "{}", error());
	let address = "127.0.0.1";
	let paired = unsafe {
		sandom_gamestream_client_pair(client, address.as_ptr(), address.len(), ports.http, PIN.as_ptr(), PIN.len(), pairing.as_ptr(), pairing.len())
	};
	assert_eq!(paired, 1, "{}", error());

	let doorbell = Box::new((std::sync::Mutex::new(0u32), std::sync::Condvar::new()));
	let context = &*doorbell as *const _ as *mut std::ffi::c_void;
	let session = unsafe {
		sandom_gamestream_client_start(client, pairing.as_ptr(), pairing.len(), 320, 192, 30, 5000, std::ptr::null_mut(), ring, context)
	};
	assert!(!session.is_null(), "{}", error());
	wait_for(&server_rx, "session start", |event| matches!(event, ServerEvent::SessionStarted { .. }).then_some(()));
	assert_eq!(unsafe { sandom_gamestream_client_ended(session, std::ptr::null_mut(), 0) }, -1);

	// Key frames until the doorbell rings for the first picture.
	let key = access_units(COLORS)[0];
	let deadline = Instant::now() + WAIT;
	let mut timestamp = 0;
	loop {
		assert!(Instant::now() < deadline, "the doorbell did not ring within {WAIT:?}");
		timestamp += 33_333;
		server.send_access_unit(key, true, timestamp).unwrap();
		let (rings, woken) = &*doorbell;
		let rung = woken.wait_timeout_while(rings.lock().unwrap(), Duration::from_millis(50), |count| *count == 0).unwrap();
		if *rung.0 > 0 {
			break;
		}
	}
	let (mut width, mut height, mut frame_index, mut rtp_timestamp) = (0, 0, 0, 0);
	let picture = unsafe { sandom_gamestream_client_picture(session, &mut width, &mut height, &mut frame_index, &mut rtp_timestamp) };
	assert!(!picture.is_null(), "{}", error());
	assert_eq!((width, height), (320, 192));
	assert!(frame_index >= 1);
	assert!(!unsafe { sandom_gamestream_client_picture_texture(picture) }.is_null());
	unsafe { sandom_gamestream_client_picture_free(picture) };

	assert_eq!(unsafe { sandom_gamestream_client_mouse_move(session, 160, 96, 320, 192) }, 1, "{}", error());
	let received = wait_for(&server_rx, "an input event", |event| match event {
		ServerEvent::Input(event) => Some(event),
		_ => None,
	});
	assert_eq!(
		received,
		InputEvent::MouseMoveAbsolute(MouseMoveAbsolute { x: 160, y: 96, screen_width: 320, screen_height: 192 })
	);
	assert_eq!(unsafe { sandom_gamestream_client_mouse_button(session, 1, 9) }, 0);
	assert!(error().contains("mouse button 9 is not 1 to 5"), "{}", error());

	assert_eq!(unsafe { sandom_gamestream_client_end(session) }, 1, "{}", error());
	wait_for(&server_rx, "the session's end", |event| matches!(event, ServerEvent::SessionEnded).then_some(()));
	unsafe { sandom_gamestream_client_free(client) };
	server.stop().unwrap();
}
