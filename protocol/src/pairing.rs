//! The identities and signatures of GameStream pairing, from Moonshine's
//! `src/tls.rs` and `src/clients.rs`.
//!
//! Each side has a self-signed RSA certificate. Pairing proves each side's
//! key with RSA PKCS#1 SHA-256 signatures, and binds the PIN to each
//! certificate through the certificate's own signature bytes, which the
//! challenge hashes include.

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_SHA256, RsaKeyPair, UnparsedPublicKey};
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose, SerialNumber};
use rsa::rand_core::RngCore;
use rsa::{
	RsaPrivateKey,
	pkcs8::{EncodePrivateKey, LineEnding},
};
use x509_parser::prelude::*;

/// A certificate and its private key, both PEM.
#[derive(Clone, Debug)]
pub struct Identity {
	pub certificate_pem: String,
	pub private_key_pem: String,
}

/// A new self-signed RSA-2048 certificate named `common_name`, valid for
/// ten years.
pub fn create_identity(common_name: &str) -> Result<Identity> {
	// rsa depends on rand_core 0.6, so it takes its bundled OsRng.
	let mut rng = rsa::rand_core::OsRng;
	let private_key = RsaPrivateKey::new(&mut rng, 2048).context("generating an RSA-2048 key")?;
	let private_key_pem = private_key.to_pkcs8_pem(LineEnding::LF).context("encoding the RSA key")?.to_string();

	let mut params = CertificateParams::default();
	params.not_before = SystemTime::now().into();
	params.not_after = (SystemTime::now() + Duration::from_secs(3650 * 24 * 60 * 60)).into();
	params.serial_number = Some(SerialNumber::from(rng.next_u64()));
	let mut distinguished_name = DistinguishedName::new();
	distinguished_name.push(DnType::CommonName, common_name);
	params.distinguished_name = distinguished_name;
	params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
	params.key_usages =
		vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment, KeyUsagePurpose::KeyAgreement];

	let key_pair = KeyPair::from_pem(&private_key_pem).context("reading the RSA key back")?;
	let certificate = params.self_signed(&key_pair).context("signing the certificate")?;
	Ok(Identity { certificate_pem: certificate.pem(), private_key_pem })
}

/// The RSA PKCS#1 SHA-256 signature of `data` with the PEM private key.
pub fn sign(data: &[u8], private_key_pem: &str) -> Result<Vec<u8>> {
	let mut reader = std::io::Cursor::new(private_key_pem.as_bytes());
	let key = rustls_pemfile::private_key(&mut reader)
		.context("parsing the private key PEM")?
		.ok_or_else(|| anyhow!("the private key PEM holds no key"))?;
	let key_pair = RsaKeyPair::from_pkcs8(key.secret_der()).map_err(|error| anyhow!("not an RSA PKCS#8 key: {error}"))?;
	let mut signature = vec![0; key_pair.public_key().modulus_len()];
	key_pair
		.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), data, &mut signature)
		.map_err(|error| anyhow!("RSA signing failed: {error}"))?;
	Ok(signature)
}

fn with_certificate<T>(certificate_pem: &str, read: impl FnOnce(&X509Certificate<'_>) -> T) -> Result<T> {
	let (_, pem) = parse_x509_pem(certificate_pem.as_bytes()).map_err(|error| anyhow!("not a PEM certificate: {error}"))?;
	let certificate = pem.parse_x509().map_err(|error| anyhow!("not an X.509 certificate: {error}"))?;
	Ok(read(&certificate))
}

/// The signature bytes of a PEM certificate, which the pairing challenge
/// hashes include.
pub fn certificate_signature(certificate_pem: &str) -> Result<Vec<u8>> {
	with_certificate(certificate_pem, |certificate| certificate.signature_value.data.to_vec())
}

/// The DER bytes of a PEM certificate.
pub fn certificate_der(certificate_pem: &str) -> Result<Vec<u8>> {
	let (_, pem) = parse_x509_pem(certificate_pem.as_bytes()).map_err(|error| anyhow!("not a PEM certificate: {error}"))?;
	Ok(pem.contents)
}

/// Checks that `signature` is the RSA PKCS#1 SHA-256 signature of `data`
/// with the key of the PEM certificate.
pub fn verify_signature(certificate_pem: &str, data: &[u8], signature: &[u8]) -> Result<()> {
	let public_key = with_certificate(certificate_pem, |certificate| {
		certificate.tbs_certificate.subject_pki.subject_public_key.data.to_vec()
	})?;
	if UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &public_key).verify(data, signature).is_err() {
		bail!("the signature does not match the certificate's public key");
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_signature_verifies_with_its_certificate_only() {
		let alice = create_identity("alice").unwrap();
		let bob = create_identity("bob").unwrap();
		let signature = sign(b"secret", &alice.private_key_pem).unwrap();
		verify_signature(&alice.certificate_pem, b"secret", &signature).unwrap();
		assert!(verify_signature(&bob.certificate_pem, b"secret", &signature).is_err());
		assert!(verify_signature(&alice.certificate_pem, b"other", &signature).is_err());
		assert!(!certificate_signature(&alice.certificate_pem).unwrap().is_empty());
	}
}
