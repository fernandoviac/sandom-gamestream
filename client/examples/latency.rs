//! Measures the time from a server's send of an access unit to the client's
//! decoded Metal texture.
//!
//! The stream is an Annex B H.264 file whose access units start with access
//! unit delimiters (libx264 `aud=1`), sent at `--fps`.
//!
//! - `latency loopback --stream FILE [--fps 60]`: server and client in one
//!   process; one clock times each frame from send to decoded texture.
//! - `latency serve --bind ADDRESS --pin PIN --stream FILE [--fps 60]
//!   [--port-offset N]` and `latency client --server ADDRESS --pin PIN
//!   [--identity DIR] [--port-offset N]`: two machines. The offset moves
//!   every GameStream port, so the server runs beside another one. For each
//!   decoded picture the client sends an absolute mouse move whose x is the
//!   frame's index; the server times each frame from its send to that
//!   input's arrival (send, decode, and the control stream's way back).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sandom_gamestream::{Server, ServerConfig, ServerEvent, ServerPorts};
use sandom_gamestream_client::h264::{access_units, is_key_frame};
use sandom_gamestream_client::input::InputEvent;
#[cfg(target_os = "macos")]
use sandom_gamestream_client::{
	ClientIdentity, Session, SessionConfig, SessionEvent, VideoOutput, input::MouseMoveAbsolute, pair,
};

fn argument(arguments: &[String], name: &str) -> Option<String> {
	arguments.iter().position(|argument| argument == name).and_then(|index| arguments.get(index + 1)).cloned()
}

fn required(arguments: &[String], name: &str) -> Result<String> {
	argument(arguments, name).with_context(|| format!("{name} is required"))
}

/// Percentiles of `durations`, in milliseconds.
fn report(what: &str, mut durations: Vec<Duration>) {
	durations.sort();
	let at = |fraction: f64| durations[((durations.len() - 1) as f64 * fraction).round() as usize].as_secs_f64() * 1000.0;
	println!(
		"{what}: {} frames; min {:.2} ms, median {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
		durations.len(),
		at(0.0),
		at(0.5),
		at(0.95),
		at(0.99),
		at(1.0)
	);
}

/// Writes one line per frame: its index, then each time in milliseconds
/// from the first send, or `-` when the frame has none.
fn write_timeline(path: &str, sent: &HashMap<u32, Instant>, arrivals: &HashMap<u32, Instant>) -> Result<()> {
	let start = *sent.values().min().context("no frame was sent")?;
	let milliseconds = |time: Option<&Instant>| time.map_or("-".to_string(), |time| format!("{:.2}", time.duration_since(start).as_secs_f64() * 1000.0));
	let mut frames: Vec<u32> = sent.keys().copied().collect();
	frames.sort();
	let lines: String = frames.iter().map(|frame| format!("{frame} {} {}\n", milliseconds(sent.get(frame)), milliseconds(arrivals.get(frame)))).collect();
	std::fs::write(path, lines).with_context(|| format!("writing the timeline {path}"))
}

/// Starts a server on `bind` that takes `pin` when a client pairs.
fn start_server(
	bind: &str,
	pin: &str,
	directory: &std::path::Path,
	port_offset: u16,
) -> Result<(Server, Receiver<ServerEvent>, ServerPorts)> {
	let defaults = ServerPorts::default();
	let ports = ServerPorts {
		http: defaults.http + port_offset,
		https: defaults.https + port_offset,
		rtsp: defaults.rtsp + port_offset,
		video: defaults.video + port_offset,
		control: defaults.control + port_offset,
		audio: defaults.audio + port_offset,
	};
	let (events, receiver) = channel();
	let (address, pin) = (bind.to_string(), pin.to_string());
	let server = Server::start(
		ServerConfig {
			host_name: "sandom-latency".to_string(),
			certificate_directory: directory.to_path_buf(),
			application_title: "Sandom".to_string(),
			bind_address: bind.to_string(),
			ports,
		},
		move |event| {
			if let ServerEvent::PinRequired { client } = &event {
				let (address, pin, client) = (address.clone(), pin.clone(), client.clone());
				std::thread::spawn(move || {
					if let Err(error) = submit_pin(&address, ports.http, &client, &pin) {
						eprintln!("submitting the PIN failed: {error:#}");
					}
				});
			}
			let _ = events.send(event);
		},
	)?;
	Ok((server, receiver, ports))
}

fn submit_pin(address: &str, port: u16, client: &str, pin: &str) -> Result<()> {
	use std::io::{Read, Write};
	let body = format!("uniqueid={client}&pin={pin}");
	let mut connection = std::net::TcpStream::connect((address, port))?;
	write!(
		connection,
		"POST /submit-pin HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
		body.len()
	)?;
	let mut response = String::new();
	connection.read_to_string(&mut response)?;
	if !response.starts_with("HTTP/1.1 200") {
		bail!("the server refused the PIN: {response}");
	}
	Ok(())
}

/// Sends the stream's access units at `fps`, from the first key frame, and
/// returns each frame number's send time. The server numbers frames from 1
/// in send order.
fn send_stream(server: &Server, units: &[&[u8]], fps: u32, first_frame: u32) -> Result<HashMap<u32, Instant>> {
	let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
	let start = Instant::now();
	let mut sent = HashMap::new();
	for (index, unit) in units.iter().enumerate() {
		let due = start + interval * index as u32;
		if let Some(wait) = due.checked_duration_since(Instant::now()) {
			std::thread::sleep(wait);
		}
		let timestamp_us = (interval * index as u32).as_micros() as i64;
		sent.insert(first_frame + index as u32, Instant::now());
		server.send_access_unit(unit, is_key_frame(unit), timestamp_us)?;
	}
	Ok(sent)
}

/// Sends key frames until the client's video PING has reached the server
/// and a picture or an input echo comes back; returns the frames sent.
fn prime(server: &Server, key: &[u8], arrived: impl Fn() -> bool) -> Result<u32> {
	let deadline = Instant::now() + Duration::from_secs(20);
	let mut sent = 0;
	while !arrived() {
		if Instant::now() > deadline {
			bail!("no picture came back within 20 s");
		}
		server.send_access_unit(key, true, 0)?;
		sent += 1;
		std::thread::sleep(Duration::from_millis(50));
	}
	Ok(sent)
}

#[cfg(target_os = "macos")]
fn loopback(arguments: &[String]) -> Result<()> {
	let stream = std::fs::read(required(arguments, "--stream")?)?;
	let fps: u32 = argument(arguments, "--fps").map_or(Ok(60), |fps| fps.parse())?;
	let units = access_units(&stream);
	if units.is_empty() || !is_key_frame(units[0]) {
		bail!("the stream has no access unit delimiters, or does not start with a key frame");
	}
	let directory = tempfile::tempdir()?;
	let (server, server_rx, ports) = start_server("127.0.0.1", "0000", &directory.path().join("server"), 0)?;
	let client = ClientIdentity::load_or_create(&directory.path().join("client"))?;
	let paired = pair(&client, "127.0.0.1", ports.http, "0000")?;
	let (pictures, picture_rx) = channel();
	let config = SessionConfig { width: 1280, height: 720, fps, video_output: VideoOutput::Metal { device: 0 }, ..SessionConfig::default() };
	let session = Session::start(&client, &paired, config, move |event| match event {
		SessionEvent::Picture(picture) => {
			let _ = pictures.send((picture.frame_index, picture.decoded_at));
		},
		SessionEvent::DecodeFailed { frame_index, reason } => eprintln!("frame {frame_index} did not decode: {reason}"),
		SessionEvent::FramesLost(lost) => eprintln!("lost frames {lost:?}"),
		SessionEvent::Ended { reason } => eprintln!("the session ended: {reason}"),
		SessionEvent::Frame(_) => {},
	})?;
	loop {
		if let ServerEvent::SessionStarted { .. } = server_rx.recv_timeout(Duration::from_secs(20))? {
			break;
		}
	}
	let primed = prime(&server, units[0], || picture_rx.try_recv().is_ok())?;
	let sent = send_stream(&server, &units, fps, primed + 1)?;
	let deadline = Instant::now() + Duration::from_secs(5);
	let mut latencies = Vec::new();
	while latencies.len() < units.len() && Instant::now() < deadline {
		if let Ok((frame, decoded)) = picture_rx.recv_timeout(Duration::from_millis(500))
			&& let Some(sent) = sent.get(&frame)
		{
			latencies.push(decoded.duration_since(*sent));
		}
	}
	report("send to decoded texture, loopback", latencies);
	session.end()?;
	server.stop()?;
	Ok(())
}

fn serve(arguments: &[String]) -> Result<()> {
	let stream = std::fs::read(required(arguments, "--stream")?)?;
	let fps: u32 = argument(arguments, "--fps").map_or(Ok(60), |fps| fps.parse())?;
	let bind = required(arguments, "--bind")?;
	let units = access_units(&stream);
	if units.is_empty() || !is_key_frame(units[0]) {
		bail!("the stream has no access unit delimiters, or does not start with a key frame");
	}
	let directory = tempfile::tempdir()?;
	let port_offset: u16 = argument(arguments, "--port-offset").map_or(Ok(0), |offset| offset.parse())?;
	let (server, server_rx, ports) = start_server(&bind, &required(arguments, "--pin")?, directory.path(), port_offset)?;
	println!("Ports: {ports:?}");
	println!("Serving on {bind}; waiting for a client's session.");
	loop {
		if let ServerEvent::SessionStarted { width, height, .. } = server_rx.recv()? {
			println!("Session started at {width} x {height}.");
			break;
		}
	}
	// Echoes arrive as absolute mouse moves whose x is the frame's index.
	let (echoes, echo_rx) = channel();
	std::thread::spawn(move || {
		for event in server_rx {
			if let ServerEvent::Input(InputEvent::MouseMoveAbsolute(echo)) = event {
				let _ = echoes.send((echo.x as u32, Instant::now()));
			}
		}
	});
	let primed = prime(&server, units[0], || echo_rx.try_recv().is_ok())?;
	let sent = send_stream(&server, &units, fps, primed + 1)?;
	let deadline = Instant::now() + Duration::from_secs(5);
	let mut round_trips = Vec::new();
	let mut arrivals = HashMap::new();
	while round_trips.len() < units.len() && Instant::now() < deadline {
		if let Ok((frame, arrived)) = echo_rx.recv_timeout(Duration::from_millis(500))
			&& let Some(sent) = sent.get(&frame)
		{
			round_trips.push(arrived.duration_since(*sent));
			arrivals.insert(frame, arrived);
		}
	}
	report("send to decoded texture to input back at the server", round_trips);
	if let Some(path) = argument(arguments, "--timeline") {
		write_timeline(&path, &sent, &arrivals)?;
	}
	server.stop()?;
	Ok(())
}

#[cfg(target_os = "macos")]
fn client(arguments: &[String]) -> Result<()> {
	let server = required(arguments, "--server")?;
	let identity = argument(arguments, "--identity").unwrap_or_else(|| "/tmp/sandom-latency-client".to_string());
	let client = ClientIdentity::load_or_create(std::path::Path::new(&identity))?;
	let port_offset: u16 = argument(arguments, "--port-offset").map_or(Ok(0), |offset| offset.parse())?;
	let paired = pair(&client, &server, ServerPorts::default().http + port_offset, &required(arguments, "--pin")?)?;
	let (ended, ended_rx) = channel();
	let session = std::sync::Arc::new(std::sync::Mutex::new(None::<Session>));
	let echo = session.clone();
	let pictures = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(u32, Instant)>::new()));
	let decoded = pictures.clone();
	let config = SessionConfig { width: 1280, height: 720, fps: 60, video_output: VideoOutput::Metal { device: 0 }, ..SessionConfig::default() };
	let started = Session::start(&client, &paired, config, move |event| match event {
		SessionEvent::Picture(picture) => {
			decoded.lock().unwrap().push((picture.frame_index, picture.decoded_at));
			if let Some(session) = echo.lock().unwrap().as_ref() {
				let move_to = MouseMoveAbsolute { x: picture.frame_index as i16, y: 0, screen_width: 1280, screen_height: 720 };
				let _ = session.send_input(InputEvent::MouseMoveAbsolute(move_to));
			}
		},
		SessionEvent::DecodeFailed { frame_index, reason } => eprintln!("frame {frame_index} did not decode: {reason}"),
		SessionEvent::FramesLost(lost) => eprintln!("lost frames {lost:?}"),
		SessionEvent::Ended { reason } => {
			let _ = ended.send(reason);
		},
		SessionEvent::Frame(_) => {},
	})?;
	*session.lock().unwrap() = Some(started);
	println!("Streaming; the session ends when the server stops.");
	println!("The session ended: {}", ended_rx.recv()?);
	let pictures = pictures.lock().unwrap();
	println!("Decoded {} pictures.", pictures.len());
	if let Some(path) = argument(arguments, "--timeline") {
		let start = pictures.first().context("no picture was decoded")?.1;
		let lines: String = pictures
			.iter()
			.map(|(frame, decoded)| format!("{frame} {:.2}\n", decoded.duration_since(start).as_secs_f64() * 1000.0))
			.collect();
		std::fs::write(&path, lines).with_context(|| format!("writing the timeline {path}"))?;
	}
	Ok(())
}

fn main() -> Result<()> {
	let arguments: Vec<String> = std::env::args().skip(1).collect();
	let flag = |name: &str| arguments.iter().any(|argument| argument == name);
	let level = if flag("--trace") {
		tracing::Level::TRACE
	} else if flag("--verbose") {
		tracing::Level::DEBUG
	} else {
		tracing::Level::INFO
	};
	tracing_subscriber::fmt().with_max_level(level).with_writer(std::io::stderr).init();
	match arguments.first().map(String::as_str) {
		#[cfg(target_os = "macos")]
		Some("loopback") => loopback(&arguments),
		Some("serve") => serve(&arguments),
		#[cfg(target_os = "macos")]
		Some("client") => client(&arguments),
		#[cfg(not(target_os = "macos"))]
		Some("loopback" | "client") => bail!("the client decodes with VideoToolbox, on macOS only"),
		_ => bail!("usage: latency loopback|serve|client …; see the source's documentation"),
	}
}
