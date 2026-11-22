//! The RTSP requests that set up a session, the counterpart of the server's
//! `src/rtsp.rs`. The server answers one request per connection, as
//! Moonlight expects, so each request opens its own connection.

use std::io::{Read, Write};
use std::net::TcpStream;

use anyhow::{Context, Result, anyhow, ensure};

use crate::http::REQUEST_TIMEOUT;

/// A response's status, headers and body.
pub struct RtspResponse {
	pub status: u16,
	headers: Vec<(String, String)>,
	pub body: String,
}

impl RtspResponse {
	pub fn header(&self, name: &str) -> Option<&str> {
		self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
	}

	/// The value of SDP attribute `name` (`a=name:value`) in the body.
	pub fn sdp_attribute(&self, name: &str) -> Option<&str> {
		let prefix = format!("a={name}:");
		self.body.lines().find_map(|line| line.trim().strip_prefix(prefix.as_str())).map(str::trim)
	}
}

/// The RTSP server of a launched session.
pub struct Rtsp {
	address: String,
	port: u16,
	sequence: u32,
}

impl Rtsp {
	pub fn new(address: &str, port: u16) -> Self {
		Self { address: address.to_string(), port, sequence: 0 }
	}

	/// Sends one request and reads its response. `target` is the request
	/// URI, `headers` the request's own headers.
	pub fn request(&mut self, method: &str, target: &str, headers: &[(&str, &str)], body: &str) -> Result<RtspResponse> {
		self.sequence += 1;
		let mut request = format!("{method} {target} RTSP/1.0\r\nCSeq: {}\r\n", self.sequence);
		for (name, value) in headers {
			request.push_str(&format!("{name}: {value}\r\n"));
		}
		if !body.is_empty() {
			request.push_str(&format!("Content-type: application/sdp\r\nContent-length: {}\r\n", body.len()));
		}
		request.push_str("\r\n");
		request.push_str(body);

		let mut stream = TcpStream::connect((self.address.as_str(), self.port))
			.with_context(|| format!("connecting to RTSP {}:{}", self.address, self.port))?;
		stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
		stream.write_all(request.as_bytes())?;
		let mut bytes = Vec::new();
		stream.read_to_end(&mut bytes).with_context(|| format!("reading the RTSP {method} response"))?;
		let text = String::from_utf8(bytes).context("the RTSP response is not UTF-8")?;
		let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
		let mut lines = head.lines();
		let status_line = lines.next().ok_or_else(|| anyhow!("the RTSP {method} response is empty"))?;
		let status = status_line
			.split_whitespace()
			.nth(1)
			.and_then(|code| code.parse().ok())
			.ok_or_else(|| anyhow!("unreadable RTSP status line: {status_line}"))?;
		let headers = lines
			.filter_map(|line| line.split_once(':'))
			.map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
			.collect();
		let response = RtspResponse { status, headers, body: body.to_string() };
		ensure!(response.status == 200, "RTSP {method} {target} failed with status {}", response.status);
		Ok(response)
	}

	/// SETUP of `stream` (audio, video or control); returns the server's port.
	pub fn setup(&mut self, stream: &str) -> Result<u16> {
		let response = self.request(
			"SETUP",
			&format!("streamid={stream}/0/0"),
			&[("Transport", "unicast;X-GS-ClientPort=50000-50001"), ("If-Modified-Since", "Thu, 01 Jan 1970 00:00:00 GMT")],
			"",
		)?;
		let transport = response.header("Transport").ok_or_else(|| anyhow!("the {stream} SETUP response has no Transport"))?;
		transport
			.split(';')
			.find_map(|part| part.trim().strip_prefix("server_port="))
			.and_then(|port| port.split('-').next())
			.and_then(|port| port.parse().ok())
			.ok_or_else(|| anyhow!("the {stream} SETUP Transport names no server port: {transport}"))
	}
}
