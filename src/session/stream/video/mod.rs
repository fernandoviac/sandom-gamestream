//! The video stream of a session.
//!
//! The configuration, format and context types are extracted from Moonshine's
//! `src/session/stream/video/mod.rs`. `VideoStream` replaces Moonshine's
//! capture and encoding pipeline: the caller hands it encoded access units,
//! and it packetizes them and sends them to the client's address, which the
//! client's PING messages give.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use async_shutdown::ShutdownManager;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::session::{SessionKeysReceiver, SessionShutdownReason};

mod gso_socket;

use gso_socket::UdpGsoSocket;
use sandom_gamestream_protocol::video::{FrameType, Packetizer, ShardBatch};

/// Configuration for the video stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoStreamConfig {
	/// Port to use for streaming video data.
	pub port: u16,

	/// What percentage of data packets should be parity packets.
	pub fec_percentage: u8,

	/// Whether to enable video stream encryption (AES-128-GCM).
	#[serde(default)]
	pub encrypt: bool,

	/// Whether to emit a WARN log when a single frame takes longer to encode and
	/// packetize than the frame budget.
	#[serde(default)]
	pub log_frame_spikes: bool,

	/// Upper bound for the client-requested video packet size, in bytes.
	///
	/// Moonlight's default packet size (1392 bytes) can exceed the path MTU
	/// over VPNs and tunnels, causing fragmented, dropped video. When non-zero,
	/// the client's `x-nv-video[0].packetSize` is clamped to this value; a
	/// smaller client request is honored. `0` disables the cap.
	///
	/// This is the stream's packet size, not a raw interface MTU: the on-wire
	/// UDP payload is `max_packet_size + 16` bytes. For example, to fit a
	/// 1420-byte WireGuard MTU over IPv4, use 1376 (1420 minus the 20-byte IP
	/// and 8-byte UDP headers and the 16-byte stream overhead).
	#[serde(default)]
	pub max_packet_size: usize,

	/// Size of the video socket's send buffer, in bytes.
	///
	/// Data in the send buffer has already left the pipeline, so it isn't
	/// covered by its frame dropping: when the host's own uplink is slower
	/// than the bitrate, up to this much video can queue in the kernel. The
	/// buffer only fills when that uplink is the bottleneck.
	///
	/// `0` sizes it to fit a keyframe burst (about eight average frames at the
	/// stream's bitrate and frame rate), keeping a larger host default. A
	/// non-zero value sets the size exactly, even below the host default: a
	/// smaller buffer lowers the worst-case queueing, at the cost of keyframes
	/// waiting for the socket to drain. The kernel caps it at
	/// `net.core.wmem_max`.
	#[serde(default)]
	pub send_buffer_size: usize,

	/// How the encoder holds the stream to its bitrate.
	#[serde(default)]
	pub rate_control: RateControlMode,
}

/// How the encoder holds the stream to its bitrate. Either way, no frame goes
/// over the per-frame budget (bitrate / frame rate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateControlMode {
	/// Constant bitrate. Some drivers (RADV) pad frames that need fewer bits
	/// with filler data up to the budget.
	#[default]
	Cbr,
	/// Variable bitrate capped at the bitrate: frames that need fewer bits are
	/// sent at their size, without filler. Needs a driver that supports VBR
	/// encoding.
	Vbr,
}

/// Smallest accepted packet size. Lower values would leave almost no room for
/// payload after the 16-byte NV video header, so a lower cap is ignored and a
/// smaller client request is rejected.
const MIN_PACKET_SIZE: usize = 200;
/// Largest accepted packet size, so one stream packet fits an IPv6 UDP datagram:
/// 65_535 - 40 (IPv6) - 8 (UDP) - 16 (stream overhead). The packetizer sizes its
/// buffers from the client's value, so an unbounded request could exhaust memory.
const MAX_PACKET_SIZE: usize = 65_471;

impl VideoStreamConfig {
	/// Whether a negotiated packet size is safe to packetize with.
	pub(crate) fn valid_packet_size(size: usize) -> bool {
		(MIN_PACKET_SIZE..=MAX_PACKET_SIZE).contains(&size)
	}

	/// Clamp a client-requested video packet size to `max_packet_size`.
	///
	/// A client only asks for what it believes it can receive, so a request
	/// smaller than the cap is honored; the cap only lowers oversized requests.
	/// A cap of `0` disables the limit.
	pub(crate) fn clamp_packet_size(&self, requested: usize) -> usize {
		if self.max_packet_size == 0 {
			return requested;
		}
		if self.max_packet_size < MIN_PACKET_SIZE {
			tracing::warn!(
				"max_packet_size {} is below the minimum of {MIN_PACKET_SIZE}, ignoring the cap.",
				self.max_packet_size
			);
			return requested;
		}
		requested.min(self.max_packet_size)
	}
}

impl Default for VideoStreamConfig {
	fn default() -> Self {
		Self {
			port: 47998,
			fec_percentage: 20,
			encrypt: false,
			log_frame_spikes: false,
			max_packet_size: 0,
			send_buffer_size: 0,
			rate_control: RateControlMode::default(),
		}
	}
}

/// Average frames a keyframe burst spans, FEC included, for sizing the
/// socket's send buffer when `send_buffer_size` is automatic.
const KEYFRAME_BURST_FRAMES: usize = 8;

/// Send buffer size that fits a keyframe burst at `bitrate` (bits per second)
/// and `fps`.
fn keyframe_burst_bytes(bitrate: usize, fps: u32) -> usize {
	let fps = usize::try_from(fps.max(1)).unwrap_or(usize::MAX);
	bitrate / 8 / fps * KEYFRAME_BURST_FRAMES
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoFormat {
	#[default]
	H264,
	Hevc,
	Av1,
}

impl TryFrom<u32> for VideoFormat {
	type Error = ();

	fn try_from(value: u32) -> Result<Self, Self::Error> {
		match value {
			0 => Ok(Self::H264),
			1 => Ok(Self::Hevc),
			2 => Ok(Self::Av1),
			_ => Err(()),
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoDynamicRange {
	#[default]
	Sdr,
	Hdr,
}

impl TryFrom<u32> for VideoDynamicRange {
	type Error = ();

	fn try_from(value: u32) -> Result<Self, Self::Error> {
		match value {
			0 => Ok(Self::Sdr),
			1 => Ok(Self::Hdr),
			_ => Err(()),
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoChromaSampling {
	#[default]
	Yuv420,
	Yuv444,
}

impl TryFrom<u32> for VideoChromaSampling {
	type Error = ();

	fn try_from(value: u32) -> Result<Self, Self::Error> {
		match value {
			0 => Ok(Self::Yuv420),
			1 => Ok(Self::Yuv444),
			_ => Err(()),
		}
	}
}

#[derive(Clone, Debug, Default)]
pub struct VideoStreamContext {
	/// Width of the video stream in pixels.
	pub width: u32,

	/// Height of the video stream in pixels.
	pub height: u32,

	/// Frames per second of the video stream.
	pub fps: u32,

	/// Size of each encoded packet in bytes.
	pub packet_size: usize,

	/// Target bitrate for the video stream in bits per second.
	pub bitrate: usize,

	/// Minimum number of FEC packets to include for each frame.
	pub minimum_fec_packets: u32,

	/// Whether to apply QoS markings to video stream packets.
	pub qos: bool,

	/// Video format to use for encoding the stream.
	pub video_format: VideoFormat,

	/// Dynamic range of the video stream.
	pub dynamic_range: VideoDynamicRange,

	/// Chroma sampling type for the video stream.
	pub chroma_sampling_type: VideoChromaSampling,

	/// Maximum number of reference frames for the video encoder.
	pub max_reference_frames: u32,

	/// Whether the client asked for full-range (0-255) rather than
	/// limited-range (16-235) luma.
	pub full_range: bool,

	/// Whether the client has enabled video encryption.
	pub encrypt_video: bool,
}


/// Packetized frames that may wait for the socket before `send_access_unit`
/// refuses the next one.
const SEND_QUEUE_FRAMES: usize = 8;

struct VideoSender {
	packetizer: Packetizer,
	/// The session's keys; a new key id gives the packetizer the new key.
	keys_rx: SessionKeysReceiver,
	key_id: Option<i64>,
	context: VideoStreamContext,
	fec_percentage: u8,
	frame_number: u32,
	sequence_number: u32,
	/// Frames packetized after the last counter reset, tagged so the socket task
	/// drops older ones.
	epoch: u32,
	packets: mpsc::Sender<(u32, ShardBatch)>,
}

/// The running video stream of a session. Cloning shares the stream.
#[derive(Clone)]
pub(crate) struct VideoStreamHandle {
	sender: Arc<Mutex<VideoSender>>,
	stop: ShutdownManager<SessionShutdownReason>,
}

impl VideoStreamHandle {
	/// Packetize one encoded access unit and queue it for the client.
	///
	/// `timestamp_us` is the frame's presentation time in microseconds; the
	/// RTP timestamp is its 90 kHz value.
	pub(crate) fn send_access_unit(&self, data: &[u8], key: bool, timestamp_us: i64) -> Result<(), String> {
		if self.stop.is_shutdown_triggered() {
			return Err("the session's video stream has stopped".to_string());
		}
		let mut sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
		let sender = &mut *sender;
		{
			let keys = sender.keys_rx.borrow();
			if sender.key_id != Some(keys.remote_input_key_id) {
				sender
					.packetizer
					.set_key(&keys.remote_input_key)
					.map_err(|error| format!("cannot encrypt the session's video: {error:#}"))?;
				sender.key_id = Some(keys.remote_input_key_id);
			}
		}
		sender.frame_number += 1;
		let rtp_timestamp = (timestamp_us.max(0) as u64 * 9 / 100) as u32;
		let shards = sender
			.packetizer
			.packetize(
				data,
				if key { FrameType::Key } else { FrameType::Predicted },
				sender.context.packet_size,
				sender.context.minimum_fec_packets,
				sender.fec_percentage,
				sender.frame_number,
				&mut sender.sequence_number,
				rtp_timestamp,
				0,
			)
			.map_err(|error| format!("failed to packetize frame {} of {} bytes: {error:#}", sender.frame_number, data.len()))?;
		sender.packets.try_send((sender.epoch, shards)).map_err(|error| match error {
			mpsc::error::TrySendError::Full(_) => format!(
				"the video send queue holds {SEND_QUEUE_FRAMES} frames that the socket has not sent; \
				 the client's connection is slower than the frame rate"
			),
			mpsc::error::TrySendError::Closed(_) => "the session's video socket has closed".to_string(),
		})
	}

	/// A resuming client expects frame numbers from 1: restart the counters and
	/// drop frames packetized before.
	pub(crate) fn reset_counters(&self) {
		let mut sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
		sender.frame_number = 0;
		sender.sequence_number = 0;
		sender.epoch += 1;
	}
}

pub(crate) struct VideoStream;

impl VideoStream {
	/// Bind the video socket and start sending to the client once it PINGs.
	pub(crate) async fn start(
		config: &VideoStreamConfig,
		address: &str,
		context: VideoStreamContext,
		keys_rx: SessionKeysReceiver,
		stop: ShutdownManager<SessionShutdownReason>,
	) -> Result<VideoStreamHandle, ()> {
		let socket = UdpGsoSocket::new(address, config.port).await?;
		if context.qos {
			let _ = socket.set_tos_v4(160);
		}
		if config.send_buffer_size == 0 {
			socket.grow_send_buffer(keyframe_burst_bytes(context.bitrate, context.fps));
		} else {
			socket.set_send_buffer(config.send_buffer_size);
		}

		let packetizer = Packetizer::new(context.encrypt_video);
		// The encoders take seconds to build and are shared by every session;
		// PLAY answers without waiting for them.
		let (fec_percentage, minimum_fec_packets) = (config.fec_percentage, context.minimum_fec_packets);
		tokio::task::spawn_blocking(move || {
			if let Err(error) = Packetizer::warm_up(fec_percentage, minimum_fec_packets) {
				tracing::error!("Building the Reed-Solomon encoders failed: {error:#}");
			}
		});
		let (packets, packet_rx) = mpsc::channel(SEND_QUEUE_FRAMES);
		let sender = VideoSender {
			packetizer,
			keys_rx,
			key_id: None,
			context,
			fec_percentage: config.fec_percentage,
			frame_number: 0,
			sequence_number: 0,
			epoch: 0,
			packets,
		};
		let handle = VideoStreamHandle {
			sender: Arc::new(Mutex::new(sender)),
			stop: stop.clone(),
		};
		spawn_video_socket(socket, packet_rx, handle.sender.clone(), stop);
		Ok(handle)
	}
}

/// Receive the client's PINGs and send each queued frame to its address.
fn spawn_video_socket(
	socket: UdpGsoSocket,
	mut packet_rx: mpsc::Receiver<(u32, ShardBatch)>,
	sender: Arc<Mutex<VideoSender>>,
	stop: ShutdownManager<SessionShutdownReason>,
) {
	tokio::spawn(async move {
		let _stop_token = stop.trigger_shutdown_token(SessionShutdownReason::VideoPacketHandlerStopped);
		let _delay_stop = stop.delay_shutdown_token();
		let mut buffer = [0u8; 1024];
		let mut client_address: Option<SocketAddr> = None;
		loop {
			tokio::select! {
				batch = stop.wrap_cancel(packet_rx.recv()) => {
					let Ok(Some((epoch, shards))) = batch else { break };
					let current_epoch = sender.lock().unwrap_or_else(PoisonError::into_inner).epoch;
					if epoch != current_epoch || shards.shard_count() == 0 {
						continue;
					}
					// Frames before the client's first PING have no address to go to.
					if let Some(address) = client_address
						&& stop.wrap_cancel(socket.send_batch(&shards, address)).await.is_err()
					{
						break;
					}
				},
				message = stop.wrap_cancel(socket.recv_from(&mut buffer)) => {
					let (length, address) = match message {
						Ok(Ok(received)) => received,
						Ok(Err(error)) => {
							tracing::warn!("Failed to receive on the video socket: {error}");
							break;
						},
						Err(_) => break,
					};
					if &buffer[..length] == b"PING" {
						client_address = Some(address);
					} else {
						tracing::warn!("Received an unknown message of {length} bytes on the video socket.");
					}
				},
			}
		}
		tracing::debug!("Video socket stopped.");
	});
}
