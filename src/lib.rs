//! A GameStream server for Moonlight clients, from Moonshine's protocol
//! modules (`LICENSE-MOONSHINE`), without Moonshine's compositor, encoder,
//! audio server, application launcher and control panel.
//!
//! The caller encodes the video. `Server::start` runs pairing, the HTTP and
//! HTTPS endpoints, RTSP, and the video and control streams on the server's
//! own runtime threads, and reports what happens through `ServerEvent`s. The
//! caller sends each encoded H.264 access unit with
//! `Server::send_access_unit`.

// Moonshine's functions return `Result<_, ()>` and log their errors.
#![allow(clippy::result_unit_err)]

use std::path::PathBuf;
use std::sync::Arc;

use async_shutdown::ShutdownManager;

pub(crate) mod clients;
pub(crate) mod crypto;
pub(crate) mod rtsp;
pub(crate) mod session;
pub(crate) mod state;
pub(crate) mod tls;
pub(crate) mod webserver;
pub mod native;

pub use session::stream::control::input::{
	InputEvent, Key, MouseButton, MouseMoveAbsolute, MouseMoveRelative, MouseScrollHorizontal, MouseScrollVertical,
	Pen, PenToolKind, PointerEventKind, Touch,
};

use session::SessionManager;
use session::stream::StreamConfig;

/// Reasons for a global shutdown of the server's tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownReason {
	/// `Server::stop`.
	AppQuit = 1,
	/// The HTTP webserver stopped.
	HttpShutdown = 2,
	/// The HTTPS webserver stopped.
	HttpsShutdown = 3,
	/// The RTSP server stopped.
	RtspShutdown = 4,
}

/// The codecs this server encodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
	H264,
}

/// The `ServerCodecModeSupport` bit of H.264 (Moonshine's `healthcheck.rs`).
const CODEC_H264: u32 = 0x00000001;

/// Something that happened on the server. The server calls the event
/// callback from its own threads; the callback must not block.
#[derive(Debug)]
pub enum ServerEvent {
	/// A client wants to pair. The user submits the PIN that Moonlight shows
	/// with `POST /submit-pin` on the HTTP port, with `uniqueid=<client>`.
	PinRequired { client: String },
	/// A session's streams run. The caller sends a key frame first.
	SessionStarted {
		width: u32,
		height: u32,
		fps: u32,
		codec: Codec,
		/// The client's bit rate, in bits per second.
		bitrate: usize,
		/// The most reference frames the client decodes; zero means any.
		max_reference_frames: u32,
		/// Whether the client expects full-range rather than limited-range luma.
		full_range: bool,
	},
	/// The client asked for a key frame.
	KeyFrameRequested,
	/// The client sent an input event.
	Input(InputEvent),
	/// The session ended.
	SessionEnded,
}

/// Where the server reports its events.
pub type EventSink = Arc<dyn Fn(ServerEvent) + Send + Sync>;

/// The ports of the server. The defaults are the GameStream ports.
#[derive(Clone, Copy, Debug)]
pub struct ServerPorts {
	pub http: u16,
	pub https: u16,
	pub rtsp: u16,
	pub video: u16,
	pub control: u16,
	pub audio: u16,
}

impl Default for ServerPorts {
	fn default() -> Self {
		Self {
			http: 47989,
			https: 47984,
			rtsp: 48010,
			video: 47998,
			control: 47999,
			audio: 48000,
		}
	}
}

/// The configuration of a server.
#[derive(Clone, Debug)]
pub struct ServerConfig {
	/// The host name that Moonlight shows.
	pub host_name: String,
	/// The directory of the server's certificate, private key and paired
	/// clients. The server creates it.
	pub certificate_directory: PathBuf,
	/// The title of the one application that Moonlight lists.
	pub application_title: String,
	/// The address that every socket binds to, such as `0.0.0.0`.
	pub bind_address: String,
	pub ports: ServerPorts,
}

/// A running GameStream server.
pub struct Server {
	runtime: tokio::runtime::Runtime,
	shutdown: ShutdownManager<ShutdownReason>,
	session_manager: SessionManager,
}

impl Server {
	/// Start the server on its own runtime threads.
	pub fn start(config: ServerConfig, events: impl Fn(ServerEvent) + Send + Sync + 'static) -> anyhow::Result<Server> {
		let events: EventSink = Arc::new(events);
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.thread_name("sandom-gamestream")
			.build()?;
		std::fs::create_dir_all(&config.certificate_directory).map_err(|error| {
			anyhow::anyhow!(
				"cannot create the certificate directory {}: {error}",
				config.certificate_directory.display()
			)
		})?;
		let certificate_path = config.certificate_directory.join("cert.pem");
		let private_key_path = config.certificate_directory.join("key.pem");
		let (certificate, private_key) = tls::load_or_create_certificate(&certificate_path, &private_key_path)
			.map_err(|()| anyhow::anyhow!("cannot load or create the certificate in {}", config.certificate_directory.display()))?;
		let client_manager = clients::ClientManager::new(
			certificate.clone(),
			private_key,
			config.certificate_directory.join("state.toml"),
		)
		.map_err(|()| anyhow::anyhow!("cannot load the paired clients from {}", config.certificate_directory.display()))?;
		let unique_id = client_manager
			.persistent_state()
			.get_uuid()
			.map_err(|()| anyhow::anyhow!("cannot read the server's unique ID"))?;

		let mut stream = StreamConfig {
			port: config.ports.rtsp,
			..Default::default()
		};
		stream.video.port = config.ports.video;
		stream.control.port = config.ports.control;
		stream.audio.port = config.ports.audio;

		let shutdown = ShutdownManager::new();
		let session_manager = SessionManager::new(stream.clone(), config.bind_address.clone(), events.clone());
		runtime.block_on(async {
			webserver::Webserver::new(
				config.host_name.clone(),
				config.bind_address.clone(),
				config.ports.rtsp,
				webserver::WebserverConfig {
					port: config.ports.http,
					port_https: config.ports.https,
					enable_pairing: true,
					certificate: certificate_path,
					private_key: private_key_path,
				},
				config.application_title.clone(),
				CODEC_H264,
				unique_id,
				certificate,
				client_manager,
				session_manager.clone(),
				events.clone(),
				shutdown.clone(),
			)
			.map_err(|()| anyhow::anyhow!("cannot start the webserver on {}", config.bind_address))?;
			rtsp::RtspServer::new(
				config.bind_address.clone(),
				config.ports.rtsp,
				stream.video.clone(),
				stream.audio.clone(),
				stream.control.clone(),
				session_manager.clone(),
				shutdown.clone(),
			);
			anyhow::Ok(())
		})?;

		Ok(Server {
			runtime,
			shutdown,
			session_manager,
		})
	}

	/// Send one encoded H.264 access unit (Annex B) of the running session.
	/// `timestamp_us` is the frame's presentation time in microseconds.
	pub fn send_access_unit(&self, data: &[u8], key: bool, timestamp_us: i64) -> anyhow::Result<()> {
		let video = self
			.session_manager
			.video()
			.ok_or_else(|| anyhow::anyhow!("no session runs; send frames between SessionStarted and SessionEnded"))?;
		video.send_access_unit(data, key, timestamp_us).map_err(anyhow::Error::msg)
	}

	/// End the running session: its streams stop, the client sees the stream
	/// end, and `SessionEnded` follows. Without a session it does nothing.
	pub fn end_session(&self) -> anyhow::Result<()> {
		self.runtime
			.block_on(self.session_manager.stop_session())
			.map_err(|()| anyhow::anyhow!("cannot stop the session"))
	}

	/// Stop the session and every server task.
	pub fn stop(self) -> anyhow::Result<()> {
		let Server {
			runtime,
			shutdown,
			session_manager,
		} = self;
		runtime.block_on(async {
			session_manager.shutdown().await;
			let _ = shutdown.trigger_shutdown(ShutdownReason::AppQuit);
			shutdown.wait_shutdown_complete().await;
		});
		drop(runtime);
		Ok(())
	}
}
