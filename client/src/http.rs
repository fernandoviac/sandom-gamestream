//! The GameStream HTTP requests: plain HTTP/1.1 GET for pairing, and HTTPS
//! with the client's certificate for the paired requests. The server's
//! certificate is pinned: the HTTPS connection accepts exactly the
//! certificate that pairing received.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};

/// How long one request may take. Pairing waits for the user to enter the
/// PIN on the device, so its first request uses `PIN_TIMEOUT`.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const PIN_TIMEOUT: Duration = Duration::from_secs(300);

/// A response's status code and body.
pub struct Response {
	pub status: u16,
	pub body: String,
}

impl Response {
	/// The text of the first `<tag>` element of an XML body.
	pub fn element(&self, tag: &str) -> Result<&str> {
		let open = format!("<{tag}>");
		let close = format!("</{tag}>");
		let start = self.body.find(&open).ok_or_else(|| anyhow!("the response has no <{tag}>: {}", self.body))? + open.len();
		let end = self.body[start..].find(&close).ok_or_else(|| anyhow!("the response's <{tag}> does not close"))?;
		Ok(&self.body[start..start + end])
	}

	/// Fails unless the status is 200 and the XML root's status code is 200.
	pub fn require_ok(self, what: &str) -> Result<Self> {
		ensure!(self.status == 200, "{what} failed with HTTP {}: {}", self.status, self.body.trim());
		if self.body.contains("status_code=") {
			ensure!(self.body.contains("status_code=\"200\""), "{what} failed: {}", self.body.trim());
		}
		Ok(self)
	}
}

fn exchange(stream: &mut impl ReadWrite, host: &str, path_and_query: &str) -> Result<Response> {
	write!(stream, "GET {path_and_query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")?;
	stream.flush()?;
	let mut bytes = Vec::new();
	match stream.read_to_end(&mut bytes) {
		Ok(_) => {},
		// A server that closes without a TLS close_notify ends the body all the same.
		Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof && !bytes.is_empty() => {},
		Err(error) => return Err(error).context("reading the HTTP response"),
	}
	let text = String::from_utf8(bytes).context("the HTTP response is not UTF-8")?;
	let (head, body) = text.split_once("\r\n\r\n").ok_or_else(|| anyhow!("the HTTP response has no end of headers"))?;
	let status_line = head.lines().next().unwrap_or_default();
	let status = status_line
		.split_whitespace()
		.nth(1)
		.and_then(|code| code.parse().ok())
		.ok_or_else(|| anyhow!("unreadable HTTP status line: {status_line}"))?;
	if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
		bail!("the server answered with a chunked body, which this client does not read");
	}
	Ok(Response { status, body: body.to_string() })
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

fn connect(address: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
	let stream = TcpStream::connect((address, port)).with_context(|| format!("connecting to {address}:{port}"))?;
	stream.set_read_timeout(Some(timeout))?;
	stream.set_write_timeout(Some(timeout))?;
	Ok(stream)
}

/// GET `path_and_query` over plain HTTP.
pub fn get(address: &str, port: u16, path_and_query: &str, timeout: Duration) -> Result<Response> {
	let mut stream = connect(address, port, timeout)?;
	exchange(&mut stream, address, path_and_query)
}

/// GET `path_and_query` over HTTPS, presenting the client's certificate and
/// accepting only `server_certificate_der`.
pub fn get_tls(
	address: &str,
	port: u16,
	path_and_query: &str,
	client_certificate_der: &[u8],
	client_private_key_pem: &str,
	server_certificate_der: &[u8],
) -> Result<Response> {
	let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
	let mut reader = std::io::Cursor::new(client_private_key_pem.as_bytes());
	let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut reader)
		.context("parsing the client's private key")?
		.ok_or_else(|| anyhow!("the client's private key PEM holds no key"))?;
	let config = ClientConfig::builder_with_provider(provider.clone())
		.with_safe_default_protocol_versions()?
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(PinnedServer {
			certificate: server_certificate_der.to_vec(),
			provider,
		}))
		.with_client_auth_cert(vec![CertificateDer::from(client_certificate_der.to_vec())], key)
		.context("the client's certificate and key do not form a TLS identity")?;
	let name = ServerName::try_from(address.to_string()).with_context(|| format!("{address} is not a server name"))?;
	let connection = ClientConnection::new(Arc::new(config), name)?;
	let mut stream = StreamOwned::new(connection, connect(address, port, REQUEST_TIMEOUT)?);
	exchange(&mut stream, address, path_and_query)
}

/// Accepts exactly one certificate: the one that pairing received.
#[derive(Debug)]
struct PinnedServer {
	certificate: Vec<u8>,
	provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinnedServer {
	fn verify_server_cert(
		&self,
		end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_server_name: &ServerName<'_>,
		_ocsp_response: &[u8],
		_now: UnixTime,
	) -> Result<ServerCertVerified, rustls::Error> {
		if end_entity.as_ref() == self.certificate.as_slice() {
			Ok(ServerCertVerified::assertion())
		} else {
			Err(rustls::Error::General(
				"the server presented a certificate other than the one it paired with".to_string(),
			))
		}
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		certificate: &CertificateDer<'_>,
		signature: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls12_signature(message, certificate, signature, &self.provider.signature_verification_algorithms)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		certificate: &CertificateDer<'_>,
		signature: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls13_signature(message, certificate, signature, &self.provider.signature_verification_algorithms)
	}

	fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
		self.provider.signature_verification_algorithms.supported_schemes()
	}
}
