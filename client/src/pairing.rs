//! The client's identity and the five steps of GameStream pairing, the
//! counterpart of the server's `src/webserver/pairing.rs` and
//! `src/clients.rs`.

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use sandom_gamestream_protocol::crypto::{ecb_decrypt, ecb_encrypt, pairing_key, sha256};
use sandom_gamestream_protocol::pairing::{
	Identity, certificate_der, certificate_signature, create_identity, sign, verify_signature,
};

use crate::http;

/// The client's certificate, key and unique ID, kept in a directory so the
/// client stays paired across runs.
#[derive(Clone, Debug)]
pub struct ClientIdentity {
	pub unique_id: String,
	pub identity: Identity,
}

impl ClientIdentity {
	/// The identity in `directory` (`cert.pem`, `key.pem`, `uniqueid`), or a
	/// new one written there.
	pub fn load_or_create(directory: &Path) -> Result<Self> {
		let (certificate, key, unique_id) =
			(directory.join("cert.pem"), directory.join("key.pem"), directory.join("uniqueid"));
		let present = [&certificate, &key, &unique_id].map(|path| path.exists());
		if present == [true; 3] {
			return Ok(Self {
				unique_id: std::fs::read_to_string(&unique_id)?.trim().to_string(),
				identity: Identity {
					certificate_pem: std::fs::read_to_string(&certificate)?,
					private_key_pem: std::fs::read_to_string(&key)?,
				},
			});
		}
		ensure!(
			present == [false; 3],
			"{} holds part of a client identity (cert.pem, key.pem, uniqueid present: {present:?}); remove it or restore the missing files",
			directory.display()
		);
		std::fs::create_dir_all(directory).with_context(|| format!("creating {}", directory.display()))?;
		let identity = create_identity("Sandom GameStream Client")?;
		let id = hex::encode_upper(random_bytes::<8>()?);
		std::fs::write(&certificate, &identity.certificate_pem)?;
		std::fs::write(&key, &identity.private_key_pem)?;
		std::fs::write(&unique_id, &id)?;
		Ok(Self { unique_id: id, identity })
	}

	pub fn certificate_der(&self) -> Result<Vec<u8>> {
		certificate_der(&self.identity.certificate_pem)
	}
}

/// A server that this client paired with.
#[derive(Clone, Debug)]
pub struct PairedServer {
	pub address: String,
	pub http_port: u16,
	pub https_port: u16,
	pub certificate_pem: String,
}

impl PairedServer {
	/// Writes the pairing to `path`: `address`, `http_port` and `https_port`
	/// lines, then the server's PEM certificate.
	pub fn save(&self, path: &Path) -> Result<()> {
		let text = format!(
			"address={}\nhttp_port={}\nhttps_port={}\n{}",
			self.address, self.http_port, self.https_port, self.certificate_pem
		);
		std::fs::write(path, text).with_context(|| format!("writing the pairing to {}", path.display()))
	}

	/// Reads a pairing that `save` wrote.
	pub fn load(path: &Path) -> Result<Self> {
		let text = std::fs::read_to_string(path).with_context(|| format!("reading the pairing {}", path.display()))?;
		let mut lines = text.splitn(4, '\n');
		let mut field = |name: &str| -> Result<String> {
			let line = lines.next().unwrap_or_default();
			line.strip_prefix(&format!("{name}="))
				.map(str::to_string)
				.ok_or_else(|| anyhow::anyhow!("{} line `{line}` is not `{name}=…`", path.display()))
		};
		let address = field("address")?;
		let http_port = field("http_port")?.parse().with_context(|| format!("{}: http_port", path.display()))?;
		let https_port = field("https_port")?.parse().with_context(|| format!("{}: https_port", path.display()))?;
		let certificate_pem = lines.next().unwrap_or_default().to_string();
		ensure!(certificate_pem.starts_with("-----BEGIN CERTIFICATE-----"), "{} holds no server certificate", path.display());
		Ok(Self { address, http_port, https_port, certificate_pem })
	}
}

pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N]> {
	let mut bytes = [0u8; N];
	SystemRandom::new().fill(&mut bytes).map_err(|_| anyhow::anyhow!("the system random source failed"))?;
	Ok(bytes)
}

/// Pairs with the server at `address`: the user enters `pin` on the
/// device. The first request waits for that, up to `http::PIN_TIMEOUT`.
pub fn pair(client: &ClientIdentity, address: &str, http_port: u16, pin: &str) -> Result<PairedServer> {
	let id = &client.unique_id;
	let salt = random_bytes::<16>()?;
	let key = pairing_key(&salt, pin);
	let request = |query: String, timeout| {
		http::get(address, http_port, &format!("/pair?uniqueid={id}&devicename=sandom&updateState=1&{query}"), timeout)
	};

	// 1. Exchange certificates; the server waits for the PIN.
	let response = request(
		format!(
			"phrase=getservercert&salt={}&clientcert={}",
			hex::encode(salt),
			hex::encode(&client.identity.certificate_pem)
		),
		http::PIN_TIMEOUT,
	)?
	.require_ok("pairing step 1 (certificates)")?;
	let server_certificate_pem = String::from_utf8(hex::decode(response.element("plaincert")?)?)
		.context("the server's certificate is not UTF-8 PEM")?;

	// 2. Challenge the server: its answer hashes our challenge with its
	// certificate's signature and its secret, which step 3 reveals.
	let client_challenge = random_bytes::<16>()?;
	let response = request(
		format!("clientchallenge={}", hex::encode(ecb_encrypt(&client_challenge, &key)?)),
		http::REQUEST_TIMEOUT,
	)?
	.require_ok("pairing step 2 (client challenge)")?;
	let answer = ecb_decrypt(&hex::decode(response.element("challengeresponse")?)?, &key)?;
	ensure!(answer.len() == 48, "the server's challenge response has {} bytes, not 48", answer.len());
	let (server_hash, server_challenge) = answer.split_at(32);

	// 3. Answer the server's challenge; the server reveals its secret, signed.
	let client_secret = random_bytes::<16>()?;
	let client_hash =
		sha256(&[server_challenge, &certificate_signature(&client.identity.certificate_pem)?, &client_secret]);
	let response = request(
		format!("serverchallengeresp={}", hex::encode(ecb_encrypt(&client_hash, &key)?)),
		http::REQUEST_TIMEOUT,
	)?
	.require_ok("pairing step 3 (server challenge response)")?;
	let pairing_secret = hex::decode(response.element("pairingsecret")?)?;
	ensure!(pairing_secret.len() > 16, "the server's pairing secret has {} bytes", pairing_secret.len());
	let (server_secret, server_signature) = pairing_secret.split_at(16);
	verify_signature(&server_certificate_pem, server_secret, server_signature)
		.context("the server's secret is not signed by its certificate's key")?;
	let expected_hash = sha256(&[&client_challenge, &certificate_signature(&server_certificate_pem)?, server_secret]);
	if expected_hash.as_slice() != server_hash {
		bail!("the server's challenge response does not match its secret: a wrong PIN, or another party in between");
	}

	// 4. and 5. The pairing challenge, then our signed secret.
	request("phrase=pairchallenge".to_string(), http::REQUEST_TIMEOUT)?.require_ok("pairing step 4 (pair challenge)")?;
	let mut signed_secret = client_secret.to_vec();
	signed_secret.extend(sign(&client_secret, &client.identity.private_key_pem)?);
	request(format!("clientpairingsecret={}", hex::encode(signed_secret)), http::REQUEST_TIMEOUT)?
		.require_ok("pairing step 5 (client pairing secret)")?;

	let serverinfo = http::get(address, http_port, &format!("/serverinfo?uniqueid={id}"), http::REQUEST_TIMEOUT)?
		.require_ok("serverinfo")?;
	let https_port = serverinfo.element("HttpsPort")?.parse().context("the server's HttpsPort is not a port")?;
	Ok(PairedServer { address: address.to_string(), http_port, https_port, certificate_pem: server_certificate_pem })
}
