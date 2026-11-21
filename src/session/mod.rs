//! Session state of a GameStream server.
//!
//! The session-key types are extracted from Moonshine's `src/session/mod.rs`.
//! `SessionManager` replaces Moonshine's session manager: it holds the
//! negotiated session parameters, starts the video and control streams when
//! RTSP PLAY arrives, and reports the session's start and end as
//! `ServerEvent`s. It launches no application and no compositor.

use std::sync::Arc;

use async_shutdown::ShutdownManager;
use tokio::sync::{Mutex, watch};

use crate::session::stream::StreamConfig;
use crate::session::stream::audio::{AudioChannels, AudioStreamContext};
use crate::session::stream::control::ControlStream;
use crate::session::stream::video::{
	VideoChromaSampling, VideoDynamicRange, VideoFormat, VideoStream, VideoStreamContext, VideoStreamHandle,
};
use crate::{Codec, EventSink, ServerEvent};

pub mod stream;

/// Raw session encryption key data.
#[derive(Clone, Debug)]
pub struct SessionKeyData {
	/// AES GCM key used for encoding video / audio / control messages.
	pub remote_input_key: Vec<u8>,

	/// AES GCM initialization vector for video / audio / control messages.
	pub remote_input_key_id: i64,
}

pub(crate) type SessionKeysReceiver = watch::Receiver<SessionKeyData>;
pub(crate) type SessionKeysSender = watch::Sender<SessionKeyData>;

/// Session keys — either raw keys or a watch receiver.
#[derive(Clone, Debug)]
pub enum SessionKeys {
	Keys(SessionKeyData),
	Rx(SessionKeysReceiver),
}

impl SessionKeys {
	pub(crate) fn new(remote_input_key: Vec<u8>, remote_input_key_id: i64) -> Self {
		Self::Keys(SessionKeyData {
			remote_input_key,
			remote_input_key_id,
		})
	}

	pub(crate) fn clone_rx(&self) -> Option<SessionKeysReceiver> {
		match self {
			Self::Rx(rx) => Some(rx.clone()),
			_ => None,
		}
	}
}

/// Why a session's streams stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionShutdownReason {
	/// The server is shutting down.
	ManagerShutdown,
	/// The client cancelled the session (`/cancel`).
	UserStopped,
	/// The video socket task stopped.
	VideoPacketHandlerStopped,
	/// The control stream stopped: the client disconnected or stopped pinging.
	ControlStreamStopped,
}

/// The parameters of a session, from `/launch`.
#[derive(Clone, Debug)]
#[allow(dead_code, reason = "the launch request's values, kept whole; the RTSP negotiation decides the stream")]
pub struct SessionContext {
	/// ID of the application as reported to the client.
	pub application_id: i32,

	/// Resolution of the video stream (width, height).
	pub resolution: (u32, u32),

	/// Refresh rate of the video stream (in Hz).
	pub refresh_rate: u32,

	/// Encryption keys for encoding traffic.
	pub keys: SessionKeys,

	/// Audio channel count (2, 6, or 8).
	pub audio_channels: AudioChannels,

	/// Audio channel mask.
	pub audio_channel_mask: u32,

	/// Whether the client asked for HDR. Sandom streams SDR only.
	pub hdr: bool,
}

/// The streams of a running session.
struct ActiveStreams {
	stop: ShutdownManager<SessionShutdownReason>,
	/// Distinguishes this session's streams from a later session's.
	generation: u64,
}

#[derive(Default)]
struct SessionManagerInner {
	/// The session from `/launch`, until it ends.
	context: Option<SessionContext>,
	/// The certificate fingerprint of the client that launched the session.
	client: Option<String>,
	keys_tx: Option<SessionKeysSender>,
	video_context: Option<VideoStreamContext>,
	audio_context: Option<AudioStreamContext>,
	streams: Option<ActiveStreams>,
}

/// The server's one session slot.
#[derive(Clone)]
pub(crate) struct SessionManager {
	inner: Arc<Mutex<SessionManagerInner>>,
	/// The video stream of the running session, for `send_access_unit`, which
	/// the caller invokes from a thread outside the server's runtime.
	video: Arc<std::sync::Mutex<Option<VideoStreamHandle>>>,
	/// The generation of the next started session's streams.
	generation: Arc<std::sync::atomic::AtomicU64>,
	config: StreamConfig,
	address: String,
	events: EventSink,
}

impl SessionManager {
	pub(crate) fn new(config: StreamConfig, address: String, events: EventSink) -> Self {
		Self {
			inner: Default::default(),
			video: Default::default(),
			generation: Default::default(),
			config,
			address,
			events,
		}
	}

	pub(crate) fn video(&self) -> Option<VideoStreamHandle> {
		self.video.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
	}

	pub(crate) async fn get_session_context(&self) -> Result<Option<SessionContext>, ()> {
		Ok(self.inner.lock().await.context.clone())
	}

	/// Store the session of a `/launch` request.
	pub(crate) async fn initialize_session(&self, mut context: SessionContext) -> Result<(), ()> {
		let mut inner = self.inner.lock().await;
		if inner.context.is_some() {
			tracing::warn!("A session already exists; rejecting the launch request.");
			return Err(());
		}
		let keys = match context.keys {
			SessionKeys::Keys(data) => data,
			SessionKeys::Rx(_) => {
				tracing::error!("Session keys already initialized as a watch receiver");
				return Err(());
			},
		};
		let (tx, rx) = watch::channel(keys);
		context.keys = SessionKeys::Rx(rx);
		inner.context = Some(context);
		inner.keys_tx = Some(tx);
		Ok(())
	}

	pub(crate) async fn set_session_client(&self, fingerprint: Option<String>) {
		let mut inner = self.inner.lock().await;
		if inner.context.is_some() {
			inner.client = fingerprint;
		}
	}

	/// Moonshine launches the application here. Sandom has no application to
	/// launch: the caller renders the document when `SessionStarted` arrives.
	pub(crate) async fn launch_session(&self) -> Result<(), ()> {
		if self.inner.lock().await.context.is_none() {
			tracing::warn!("LaunchSession rejected: no session");
			return Err(());
		}
		Ok(())
	}

	/// The stream parameters of RTSP ANNOUNCE.
	pub(crate) async fn set_stream_context(
		&self,
		video_context: VideoStreamContext,
		audio_context: AudioStreamContext,
	) -> Result<(), ()> {
		let mut inner = self.inner.lock().await;
		if inner.context.is_none() {
			tracing::warn!("SetStreamContext rejected: no session");
			return Err(());
		}
		if inner.streams.is_some() {
			// A resuming client announces again; its streams keep running and
			// learn its new address from its PINGs.
			return Ok(());
		}
		inner.video_context = Some(video_context);
		inner.audio_context = Some(audio_context);
		Ok(())
	}

	/// RTSP PLAY: start the video and control streams and report `SessionStarted`.
	pub(crate) async fn start_session(&self) -> Result<(), ()> {
		let mut inner = self.inner.lock().await;
		if inner.context.is_none() {
			tracing::warn!("StartSession rejected: no session");
			return Err(());
		}
		if inner.streams.is_some() {
			// Resume: a fresh client expects frame numbers from 1 and a key frame.
			if let Some(video) = self.video() {
				video.reset_counters();
			}
			(self.events)(ServerEvent::KeyFrameRequested);
			return Ok(());
		}
		let video_context = inner.video_context.take().ok_or_else(|| tracing::error!("No video stream context"))?;
		inner.audio_context.take().ok_or_else(|| tracing::error!("No audio stream context"))?;
		let keys_rx = inner
			.context
			.as_ref()
			.and_then(|context| context.keys.clone_rx())
			.ok_or_else(|| tracing::error!("Session keys not initialized"))?;
		let codec = match video_context.video_format {
			VideoFormat::H264 => Codec::H264,
			format => {
				tracing::error!("The client negotiated {format:?}; this server encodes H.264 only.");
				return Err(());
			},
		};
		if video_context.dynamic_range != VideoDynamicRange::Sdr
			|| video_context.chroma_sampling_type != VideoChromaSampling::Yuv420
		{
			tracing::error!(
				"The client negotiated {:?} {:?}; this server encodes SDR 4:2:0 only. Turn off HDR and YUV 4:4:4 in Moonlight.",
				video_context.dynamic_range,
				video_context.chroma_sampling_type
			);
			return Err(());
		}

		let stop = ShutdownManager::new();
		let video = VideoStream::start(
			&self.config.video,
			&self.address,
			video_context.clone(),
			keys_rx.clone(),
			stop.clone(),
		)
		.await?;
		let control = ControlStream::new(&self.config.control, &self.address, stop.clone())?;
		control.start(self.config.timeout, keys_rx, self.events.clone());
		*self.video.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(video);
		let generation = self.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		inner.streams = Some(ActiveStreams { stop: stop.clone(), generation });
		drop(inner);

		// When a stream stops on its own (the client disconnects or stops
		// pinging), end this session, unless another one replaced it.
		let manager = self.clone();
		tokio::spawn(async move {
			stop.wait_shutdown_triggered().await;
			manager.end_session(Some(generation)).await;
		});

		(self.events)(ServerEvent::SessionStarted {
			width: video_context.width,
			height: video_context.height,
			fps: video_context.fps,
			codec,
			bitrate: video_context.bitrate,
			max_reference_frames: video_context.max_reference_frames,
			full_range: video_context.full_range,
		});
		Ok(())
	}

	/// `/cancel`: stop the streams and end the session.
	pub(crate) async fn stop_session(&self) -> Result<(), ()> {
		let stop = self.inner.lock().await.streams.as_ref().map(|streams| streams.stop.clone());
		match stop {
			Some(stop) => {
				let _ = stop.trigger_shutdown(SessionShutdownReason::UserStopped);
				stop.wait_shutdown_complete().await;
			},
			None => {},
		}
		self.end_session(None).await;
		Ok(())
	}

	/// Clear the session slot, and report `SessionEnded` once if streams ran.
	/// With `generation`, only the streams of that start end.
	async fn end_session(&self, generation: Option<u64>) {
		let mut inner = self.inner.lock().await;
		if let (Some(generation), Some(streams)) = (generation, inner.streams.as_ref())
			&& streams.generation != generation
		{
			return;
		}
		if generation.is_some() && inner.streams.is_none() {
			return;
		}
		let had_streams = inner.streams.take().is_some();
		*inner = SessionManagerInner::default();
		drop(inner);
		*self.video.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
		if had_streams {
			(self.events)(ServerEvent::SessionEnded);
		}
	}

	/// `/resume`: new keys for the running session.
	pub(crate) async fn update_keys(&self, keys: SessionKeyData) -> Result<(), ()> {
		let inner = self.inner.lock().await;
		if inner.streams.is_none() {
			tracing::warn!("No running session to update keys for.");
			return Err(());
		}
		match &inner.keys_tx {
			Some(keys_tx) => {
				keys_tx.send_replace(keys);
				Ok(())
			},
			None => Err(()),
		}
	}

	/// Stop the running session because the server stops.
	pub(crate) async fn shutdown(&self) {
		let stop = self.inner.lock().await.streams.as_ref().map(|streams| streams.stop.clone());
		if let Some(stop) = stop {
			let _ = stop.trigger_shutdown(SessionShutdownReason::ManagerShutdown);
			stop.wait_shutdown_complete().await;
		}
		self.end_session(None).await;
	}
}
