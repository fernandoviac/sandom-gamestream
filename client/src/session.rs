//! A streaming session with a paired server: launch over HTTPS, the RTSP
//! setup, the encrypted control stream over ENet, and the video stream over
//! UDP.
//!
//! The session reports what happens through its event callback, from its
//! own runtime threads; the callback must not block. Input goes to the
//! server through `send_input`, which queues it for the control task.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use sandom_gamestream_protocol::control::{self, ControlMessage, Direction};
use sandom_gamestream_protocol::input::InputEvent;
use sandom_gamestream_protocol::video::{Frame, FrameAssembler};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio_enet::{Event, Host, HostConfig, Packet, PacketMode, PeerState};

use crate::http;
use crate::pairing::{ClientIdentity, PairedServer, random_bytes};
#[cfg(target_os = "macos")]
use crate::decoder::{Decoder, Picture};
use crate::rtsp::Rtsp;

/// The application ID of a Sandom GameStream server's one application.
const APPLICATION_ID: u32 = 1;
/// The bit of `x-ss-general.encryptionEnabled` that encrypts video.
const ENCRYPT_VIDEO: u32 = 0x02;
/// How often the control stream pings the server, which ends a session
/// after 60 seconds without a ping. The ping is the protocol's keepalive.
const PING_INTERVAL: Duration = Duration::from_secs(1);
/// How long one ENet service call waits for an event before the control
/// task takes queued input again.
const SERVICE_TIMEOUT: Duration = Duration::from_millis(5);
/// How long the ENet connection to the control stream may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// What the session does with the video it receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoOutput {
	/// Each H.264 access unit as `SessionEvent::Frame`.
	AccessUnits,
	/// Each frame decoded by VideoToolbox into a BGRA Metal texture of the
	/// `id<MTLDevice>` at `device` (0: the system's default device), as
	/// `SessionEvent::Picture` and as `Session::latest_picture`.
	#[cfg(target_os = "macos")]
	Metal { device: usize },
}

/// The stream a session asks for.
#[derive(Clone, Copy, Debug)]
pub struct SessionConfig {
	pub width: u32,
	pub height: u32,
	pub fps: u32,
	pub bitrate_kbps: u32,
	/// The size of a video packet's NV header and payload, as Moonlight
	/// asks for it. A datagram is this plus 16 bytes, and 32 more when
	/// encrypted, and the server's socket does not fragment. The default,
	/// 1024, fits the IPv6 minimum MTU of 1280, which tunnels such as
	/// Tailscale use; 1392 fits a 1500-byte LAN.
	pub packet_size: u32,
	pub encrypt_video: bool,
	pub video_output: VideoOutput,
}

impl Default for SessionConfig {
	fn default() -> Self {
		Self {
			width: 1280,
			height: 720,
			fps: 60,
			bitrate_kbps: 10_000,
			packet_size: 1024,
			encrypt_video: true,
			video_output: VideoOutput::AccessUnits,
		}
	}
}

/// What happens in a session.
#[derive(Debug)]
pub enum SessionEvent {
	/// One H.264 access unit (`VideoOutput::AccessUnits`).
	Frame(Frame),
	/// One decoded picture (`VideoOutput::Metal`).
	#[cfg(target_os = "macos")]
	Picture(Arc<Picture>),
	/// A frame the decoder refused; the session asked the server for a key
	/// frame.
	DecodeFailed { frame_index: u32, reason: String },
	/// Frames that a later frame overtook; the session asked the server for
	/// a key frame.
	FramesLost(Vec<u32>),
	/// The session ended: the server ended it, or the connection failed.
	Ended { reason: String },
}

/// The session's latest decoded picture.
#[cfg(target_os = "macos")]
type LatestPicture = Arc<std::sync::Mutex<Option<Arc<Picture>>>>;
#[cfg(not(target_os = "macos"))]
type LatestPicture = ();

enum Command {
	Input(InputEvent),
	/// The decoder lost a reference: ask the server for a key frame.
	RequestKeyFrame,
	Stop,
}

/// A running session. `end` ends it on the server; dropping a session only
/// disconnects, and the server ends its session after 60 seconds without a
/// ping.
pub struct Session {
	runtime: Option<tokio::runtime::Runtime>,
	commands: mpsc::UnboundedSender<Command>,
	stopped: watch::Sender<bool>,
	video_encrypted: bool,
	#[cfg(target_os = "macos")]
	latest: LatestPicture,
	cancel: CancelRequest,
}

/// What `/cancel` needs: the paired HTTPS connection.
struct CancelRequest {
	address: String,
	https_port: u16,
	unique_id: String,
	client_certificate_der: Vec<u8>,
	client_private_key_pem: String,
	server_certificate_der: Vec<u8>,
}

impl Session {
	/// Launches the server's application and starts streaming. `events`
	/// receives every frame and the end of the session.
	pub fn start(
		client: &ClientIdentity,
		server: &PairedServer,
		config: SessionConfig,
		events: impl Fn(SessionEvent) + Send + Sync + 'static,
	) -> Result<Session> {
		let events: Arc<dyn Fn(SessionEvent) + Send + Sync> = Arc::new(events);
		let remote_input_key = random_bytes::<16>()?;
		let remote_input_key_id = u32::from_be_bytes(random_bytes::<4>()?) >> 1;

		// The server's address that answers: a name can resolve to addresses
		// without a route, which TCP passes over and UDP does not.
		let server_ip = reachable_address(&server.address, server.https_port)?;

		// Launch: the server starts a session with our key.
		let client_certificate_der = client.certificate_der()?;
		let server_certificate_der = sandom_gamestream_protocol::pairing::certificate_der(&server.certificate_pem)?;
		let launch = http::get_tls(
			&server.address,
			server.https_port,
			&format!(
				"/launch?uniqueid={}&appid={APPLICATION_ID}&mode={}x{}x{}&additionalStates=1&sops=0&rikey={}&rikeyid={remote_input_key_id}&localAudioPlayMode=0&surroundAudioInfo=196610",
				client.unique_id,
				config.width,
				config.height,
				config.fps,
				hex::encode(remote_input_key),
			),
			&client_certificate_der,
			&client.identity.private_key_pem,
			&server_certificate_der,
		)?
		.require_ok("launch")?;
		let session_url = launch.element("sessionUrl0")?.to_string();
		let rtsp_port: u16 = session_url
			.rsplit(':')
			.next()
			.and_then(|port| port.parse().ok())
			.ok_or_else(|| anyhow!("the session URL {session_url} names no port"))?;

		// RTSP: the stream ports, our stream parameters, then PLAY.
		let rtsp_address = SocketAddr::new(server_ip, rtsp_port);
		let mut rtsp = Rtsp::new(&server_ip.to_string(), rtsp_port);
		let target = format!("rtsp://{rtsp_address}");
		rtsp.request("OPTIONS", &target, &[], "")?;
		let description = rtsp.request("DESCRIBE", &target, &[("Accept", "application/sdp")], "")?;
		// Video is encrypted when the client asks for it and the server
		// supports it, as Moonlight negotiates it.
		let supported: u32 = match description.sdp_attribute("x-ss-general.encryptionSupported") {
			Some(flags) => flags.parse().with_context(|| format!("the server's encryptionSupported is {flags:?}"))?,
			None => 0,
		};
		let video_encrypted = config.encrypt_video && supported & ENCRYPT_VIDEO != 0;
		let _audio_port = rtsp.setup("audio")?;
		let video_port = rtsp.setup("video")?;
		let control_port = rtsp.setup("control")?;
		rtsp.request("ANNOUNCE", "streamid=control/13/0", &[], &announcement(&server.address, &config, video_encrypted))?;
		rtsp.request("PLAY", "/", &[], "")?;

		let video_address = SocketAddr::new(server_ip, video_port);
		let control_address = SocketAddr::new(server_ip, control_port);
		let video_key = video_encrypted.then_some(remote_input_key);

		let runtime = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.thread_name("sandom-gamestream-client")
			.build()?;
		let (commands, command_rx) = mpsc::unbounded_channel();
		let (stopped, stopped_rx) = watch::channel(false);
		let host = runtime.block_on(connect_control(control_address))?;
		let video = runtime.block_on(UdpSocket::bind(if video_address.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }))?;
		runtime.block_on(video.connect(video_address))?;

		let key_requests = commands.clone();
		let latest = LatestPicture::default();
		runtime.spawn(run_control(host, remote_input_key, command_rx, stopped_rx.clone(), events.clone()));
		runtime.spawn(run_video(video, video_key, config.video_output, latest.clone(), key_requests, stopped_rx, events));
		Ok(Session {
			runtime: Some(runtime),
			commands,
			stopped,
			video_encrypted,
			#[cfg(target_os = "macos")]
			latest,
			cancel: CancelRequest {
				address: server.address.clone(),
				https_port: server.https_port,
				unique_id: client.unique_id.clone(),
				client_certificate_der,
				client_private_key_pem: client.identity.private_key_pem.clone(),
				server_certificate_der,
			},
		})
	}

	/// The latest decoded picture of a `VideoOutput::Metal` session.
	#[cfg(target_os = "macos")]
	pub fn latest_picture(&self) -> Option<Arc<Picture>> {
		self.latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
	}

	/// Whether the session's video is encrypted: the client asked for it and
	/// the server supports it.
	pub fn video_encrypted(&self) -> bool {
		self.video_encrypted
	}

	/// Queues `event` for the server.
	pub fn send_input(&self, event: InputEvent) -> Result<()> {
		self.commands.send(Command::Input(event)).map_err(|_| anyhow!("the session has ended; its input goes nowhere"))
	}

	/// Ends the session: the streams stop, and `/cancel` ends the server's
	/// session.
	pub fn end(mut self) -> Result<()> {
		self.shut_down();
		let cancel = &self.cancel;
		http::get_tls(
			&cancel.address,
			cancel.https_port,
			&format!("/cancel?uniqueid={}", cancel.unique_id),
			&cancel.client_certificate_der,
			&cancel.client_private_key_pem,
			&cancel.server_certificate_der,
		)?
		.require_ok("cancel")?;
		Ok(())
	}

	fn shut_down(&mut self) {
		let _ = self.commands.send(Command::Stop);
		let _ = self.stopped.send(true);
		if let Some(runtime) = self.runtime.take() {
			runtime.shutdown_timeout(Duration::from_secs(2));
		}
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		self.shut_down();
	}
}

/// The first of `address`'s IP addresses that accepts a TCP connection on
/// `port`.
fn reachable_address(address: &str, port: u16) -> Result<IpAddr> {
	let connection = std::net::TcpStream::connect((address, port))
		.with_context(|| format!("no address of {address} accepts a connection on port {port}"))?;
	Ok(connection.peer_addr()?.ip())
}

/// The SDP of the ANNOUNCE request: the attributes the server reads.
fn announcement(address: &str, config: &SessionConfig, video_encrypted: bool) -> String {
	let encryption = if video_encrypted { ENCRYPT_VIDEO } else { 0 };
	let attributes = [
		("x-nv-video[0].clientViewportWd", config.width.to_string()),
		("x-nv-video[0].clientViewportHt", config.height.to_string()),
		("x-nv-video[0].maxFPS", config.fps.to_string()),
		("x-nv-video[0].packetSize", config.packet_size.to_string()),
		("x-ml-video.configuredBitrateKbps", config.bitrate_kbps.to_string()),
		("x-nv-vqos[0].fec.minRequiredFecPackets", "2".to_string()),
		("x-nv-vqos[0].qosTrafficType", "0".to_string()),
		("x-nv-vqos[0].bitStreamFormat", "0".to_string()),
		("x-nv-aqos.packetDuration", "5".to_string()),
		("x-nv-aqos.qosTrafficType", "0".to_string()),
		("x-ss-general.encryptionEnabled", encryption.to_string()),
	];
	let mut sdp = format!("v=0\r\no=android 0 14 IN IPv4 {address}\r\ns=NVIDIA Streaming Client\r\n");
	for (name, value) in attributes {
		sdp.push_str(&format!("a={name}:{value} \r\n"));
	}
	sdp.push_str("t=0 0\r\nm=video 47998  \r\n");
	sdp
}

async fn connect_control(address: SocketAddr) -> Result<(Host, tokio_enet::PeerId)> {
	// Without an address, ENet binds an IPv4 socket, which cannot reach an
	// IPv6 server.
	let local: SocketAddr = if address.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().expect("an unspecified address");
	let mut host = Host::new(HostConfig { address: Some(local), peer_count: 1, channel_limit: 0x30, ..Default::default() })
		.map_err(|error| anyhow!("cannot create the control stream's ENet host: {error}"))?;
	let peer = host.connect(address, 0x30, 0).map_err(|error| anyhow!("cannot connect to control {address}: {error}"))?;
	let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
	loop {
		ensure!(tokio::time::Instant::now() < deadline, "the control stream at {address} did not connect within {CONNECT_TIMEOUT:?}");
		match host.service(SERVICE_TIMEOUT).await.map_err(|error| anyhow!("control stream: {error}"))? {
			Some(Event::Connect { peer_id, .. }) if peer_id == peer => return Ok((host, peer)),
			Some(Event::Disconnect { .. }) => bail!("the server refused the control stream at {address}"),
			_ => {},
		}
	}
}

struct ControlSender {
	key: [u8; 16],
	sequence_number: u32,
}

impl ControlSender {
	fn send(&mut self, host: &mut Host, peer: tokio_enet::PeerId, plaintext: &[u8]) -> Result<()> {
		let message = control::encrypt(&self.key, self.sequence_number, Direction::ClientToHost, plaintext)?;
		self.sequence_number += 1;
		let peer = host.peer_mut(peer).ok_or_else(|| anyhow!("the control stream's peer is gone"))?;
		ensure!(peer.state() == PeerState::Connected, "the control stream is {:?}, not connected", peer.state());
		peer.send(0, Packet::new(&message, PacketMode::ReliableSequenced))
			.map_err(|error| anyhow!("sending on the control stream failed: {error}"))
	}
}

/// What ends the control task.
enum Ending {
	Stopped,
	Ended(String),
}

async fn run_control(
	(mut host, peer): (Host, tokio_enet::PeerId),
	key: [u8; 16],
	mut commands: mpsc::UnboundedReceiver<Command>,
	mut stopped: watch::Receiver<bool>,
	events: Arc<dyn Fn(SessionEvent) + Send + Sync>,
) {
	let mut sender = ControlSender { key, sequence_number: 0 };
	let ending: Result<Ending> = async {
		sender.send(&mut host, peer, &control::message(control::START_B, &[]))?;
		let mut ping = tokio::time::interval(PING_INTERVAL);
		loop {
			tokio::select! {
				command = commands.recv() => match command {
					Some(Command::Input(event)) => sender.send(&mut host, peer, &control::input_message(&event.to_bytes()))?,
					Some(Command::RequestKeyFrame) => sender.send(&mut host, peer, &control::message(control::REQUEST_IDR_FRAME, &[]))?,
					Some(Command::Stop) | None => return Ok(Ending::Stopped),
				},
				_ = ping.tick() => sender.send(&mut host, peer, &control::message(control::PING, &[]))?,
				_ = stopped.changed() => return Ok(Ending::Stopped),
				event = host.service(SERVICE_TIMEOUT) => match event.map_err(|error| anyhow!("control stream: {error}"))? {
					Some(Event::Receive { packet, .. }) => {
						if let Some(reason) = server_message(&key, packet.data())? {
							return Ok(Ending::Ended(reason));
						}
					},
					Some(Event::Disconnect { .. }) => return Ok(Ending::Ended("the server closed the control stream".to_string())),
					Some(Event::Connect { .. }) | None => {},
				},
			}
		}
	}
	.await;
	match ending {
		Ok(Ending::Stopped) => {
			if let Some(peer) = host.peer_mut(peer) {
				peer.disconnect(0);
			}
			let _ = host.flush().await;
		},
		Ok(Ending::Ended(reason)) => events(SessionEvent::Ended { reason }),
		Err(error) => events(SessionEvent::Ended { reason: format!("{error:#}") }),
	}
}

/// Reads one message from the server; returns the reason when it ends the
/// session.
fn server_message(key: &[u8], data: &[u8]) -> Result<Option<String>> {
	let message = ControlMessage::from_bytes(data)?;
	let plaintext;
	let message = match message {
		ControlMessage::Encrypted { sequence_number, tag, ciphertext } => {
			plaintext = control::decrypt(key, Direction::HostToClient, sequence_number, &tag, ciphertext)?;
			ControlMessage::from_bytes(&plaintext)?
		},
		message => message,
	};
	Ok(match message {
		ControlMessage::TerminationExtended { error_code } if error_code == control::TERMINATION_GRACEFUL => {
			Some("the server ended the session".to_string())
		},
		ControlMessage::TerminationExtended { error_code } => Some(format!("the server ended the session with error 0x{error_code:08x}")),
		_ => None,
	})
}

async fn run_video(
	socket: UdpSocket,
	key: Option<[u8; 16]>,
	output: VideoOutput,
	latest: LatestPicture,
	commands: mpsc::UnboundedSender<Command>,
	mut stopped: watch::Receiver<bool>,
	events: Arc<dyn Fn(SessionEvent) + Send + Sync>,
) {
	let result: Result<()> = async {
		let mut assembler = FrameAssembler::new(key.as_ref().map(|key| key.as_slice()))?;
		#[cfg(target_os = "macos")]
		let mut decoder = match output {
			VideoOutput::Metal { device } => Some(Decoder::new(device as *mut std::ffi::c_void)?),
			VideoOutput::AccessUnits => None,
		};
		#[cfg(not(target_os = "macos"))]
		let _ = (output, latest);
		// The server sends video to the address that PINGs its video port.
		socket.send(b"PING").await.context("pinging the video stream")?;
		let mut buffer = vec![0u8; 65536];
		loop {
			let length = tokio::select! {
				received = socket.recv(&mut buffer) => received.context("receiving video")?,
				_ = stopped.changed() => return Ok(()),
			};
			let assembled = match assembler.push(&buffer[..length]) {
				Ok(assembled) => assembled,
				Err(error) => {
					tracing::warn!("Skipped a video shard: {error:#}");
					continue;
				},
			};
			if !assembled.lost.is_empty() {
				let _ = commands.send(Command::RequestKeyFrame);
				events(SessionEvent::FramesLost(assembled.lost));
			}
			for frame in assembled.frames {
				#[cfg(target_os = "macos")]
				if let Some(decoder) = decoder.as_mut() {
					match decoder.decode(&frame) {
						Ok(picture) => {
							let picture = Arc::new(picture);
							*latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(picture.clone());
							events(SessionEvent::Picture(picture));
						},
						Err(error) => {
							let _ = commands.send(Command::RequestKeyFrame);
							events(SessionEvent::DecodeFailed { frame_index: frame.frame_index, reason: format!("{error:#}") });
						},
					}
					continue;
				}
				events(SessionEvent::Frame(frame));
			}
		}
	}
	.await;
	if let Err(error) = result {
		events(SessionEvent::Ended { reason: format!("{error:#}") });
	}
}
