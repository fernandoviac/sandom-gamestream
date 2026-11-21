//! Audio negotiation types, extracted from Moonshine's `src/session/stream/audio/mod.rs`.
//! Sandom sends no audio; RTSP still negotiates the audio stream.

use std::fmt;

use serde::{Deserialize, Serialize};
use strum_macros::Display;

/// Configuration for the audio stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioStreamConfig {
	/// Port to use for streaming audio data.
	pub port: u16,
}

impl Default for AudioStreamConfig {
	fn default() -> Self {
		Self { port: 48000 }
	}
}

/// Number of audio channels requested by the client.
#[derive(Clone, Copy, Debug, Default, Display, PartialEq, Eq, PartialOrd)]
pub enum AudioChannels {
	#[default]
	Stereo = 2,
	Surround51 = 6,
	Surround71 = 8,
}

/// The client asked for a channel count the sink does not support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioChannelError(pub u8);

impl fmt::Display for AudioChannelError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Unsupported number of channels: {}", self.0)
	}
}

impl std::error::Error for AudioChannelError {}

impl TryFrom<u8> for AudioChannels {
	type Error = AudioChannelError;

	fn try_from(val: u8) -> Result<Self, Self::Error> {
		match val {
			2 => Ok(Self::Stereo),
			6 => Ok(Self::Surround51),
			8 => Ok(Self::Surround71),
			_ => Err(AudioChannelError(val)),
		}
	}
}

impl AudioChannels {
	/// Parse the channel count a client asked for, warning and falling back to stereo when the
	/// sink does not support it.
	pub fn from_wire_or_stereo(count: u8) -> Self {
		Self::try_from(count).unwrap_or_else(|err| {
			tracing::warn!("{err}, falling back to stereo");
			Self::Stereo
		})
	}

}

/// Opus multistream configuration for a specific channel layout.
#[derive(Clone, Debug)]
#[allow(dead_code, reason = "the server answers the audio negotiation and sends no audio")]
pub struct OpusStreamConfig {
	pub channels: AudioChannels,
	pub streams: u8,
	pub coupled_streams: u8,
	pub mapping: [u8; 8],
	pub bitrate: u32,
}

/// Pre-defined Opus stream configurations matching Sunshine's behavior.
pub(crate) const OPUS_STEREO: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Stereo,
	streams: 1,
	coupled_streams: 1,
	mapping: [0, 1, 0, 0, 0, 0, 0, 0],
	bitrate: 96_000,
};

pub(crate) const OPUS_HIGH_STEREO: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Stereo,
	streams: 1,
	coupled_streams: 1,
	mapping: [0, 1, 0, 0, 0, 0, 0, 0],
	bitrate: 512_000,
};

pub(crate) const OPUS_SURROUND51: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Surround51,
	streams: 4,
	coupled_streams: 2,
	mapping: [0, 1, 4, 5, 2, 3, 0, 0],
	bitrate: 256_000,
};

pub(crate) const OPUS_HIGH_SURROUND51: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Surround51,
	streams: 6,
	coupled_streams: 0,
	mapping: [0, 1, 2, 3, 4, 5, 0, 0],
	bitrate: 1_536_000,
};

pub(crate) const OPUS_SURROUND71: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Surround71,
	streams: 5,
	coupled_streams: 3,
	mapping: [0, 1, 4, 5, 6, 7, 2, 3],
	bitrate: 450_000,
};

pub(crate) const OPUS_HIGH_SURROUND71: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Surround71,
	streams: 8,
	coupled_streams: 0,
	mapping: [0, 1, 2, 3, 4, 5, 6, 7],
	bitrate: 2_048_000,
};

/// All standard configurations, ordered for RTSP DESCRIBE emission.
pub(crate) const ALL_AUDIO_CONFIGS: [&OpusStreamConfig; 6] = [
	&OPUS_STEREO,
	&OPUS_HIGH_STEREO,
	&OPUS_SURROUND51,
	&OPUS_HIGH_SURROUND51,
	&OPUS_SURROUND71,
	&OPUS_HIGH_SURROUND71,
];

/// Audio configuration negotiated between client and server.
#[derive(Clone, Debug)]
pub struct AudioConfig {
	pub channels: AudioChannels,
	pub channel_mask: u32,
	pub high_quality: bool,
	pub stream_config: OpusStreamConfig,
}

impl Default for AudioConfig {
	fn default() -> Self {
		Self {
			channels: AudioChannels::default(),
			channel_mask: 0x3,
			high_quality: true,
			stream_config: OPUS_HIGH_STEREO,
		}
	}
}

impl AudioConfig {
	/// Select the appropriate OpusStreamConfig based on channel count and quality.
	pub fn from_channels(channels: AudioChannels, channel_mask: u32, high_quality: bool) -> Self {
		let stream_config = match (channels, high_quality) {
			(AudioChannels::Surround51, false) => OPUS_SURROUND51,
			(AudioChannels::Surround51, true) => OPUS_HIGH_SURROUND51,
			(AudioChannels::Surround71, false) => OPUS_SURROUND71,
			(AudioChannels::Surround71, true) => OPUS_HIGH_SURROUND71,
			(_, false) => OPUS_STEREO,
			(_, true) => OPUS_HIGH_STEREO,
		};
		Self {
			channels,
			channel_mask,
			high_quality,
			stream_config,
		}
	}
}

#[derive(Clone, Default)]
#[allow(dead_code, reason = "the server answers the audio negotiation and sends no audio")]
pub struct AudioStreamContext {
	/// Duration of each audio packet in milliseconds, typically 20ms for Opus.
	pub packet_duration_ms: u32,
	/// Whether to enable QoS on the audio socket.
	pub qos: bool,
	/// Negotiated audio configuration for the stream.
	pub audio_config: AudioConfig,
	/// Whether the client has enabled audio encryption.
	pub encrypt_audio: bool,
}
