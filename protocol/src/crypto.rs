//! The ciphers of the GameStream protocol: AES-128-GCM for the control and
//! video streams, AES-128-ECB for pairing, and the pairing key. The GCM
//! functions come from Moonshine's `src/crypto.rs`; the ECB functions and
//! the key from its `src/clients.rs`.

use aes::Aes128;
use aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit as _};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Key, Nonce};
use anyhow::{Result, anyhow, ensure};
use sha2::{Digest, Sha256};

/// The length of a GCM tag.
pub const TAG_LENGTH: usize = 16;

/// Encrypts `plaintext` with AES-128-GCM, and returns the ciphertext and its
/// detached tag.
pub fn gcm_encrypt(plaintext: &[u8], key: &[u8], iv: &[u8; 12]) -> Result<(Vec<u8>, [u8; TAG_LENGTH])> {
	ensure!(key.len() == 16, "an AES-128 key has 16 bytes, got {}", key.len());
	let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(key));
	let mut ciphertext = cipher
		.encrypt(Nonce::from_slice(iv), plaintext)
		.map_err(|_| anyhow!("AES-GCM refused to encrypt {} bytes", plaintext.len()))?;
	let tag_start = ciphertext.len() - TAG_LENGTH;
	let tag = ciphertext[tag_start..].try_into().expect("the tag has 16 bytes");
	ciphertext.truncate(tag_start);
	Ok((ciphertext, tag))
}

/// Decrypts `ciphertext` with AES-128-GCM and its detached `tag`.
pub fn gcm_decrypt(ciphertext: &[u8], key: &[u8], iv: &[u8; 12], tag: &[u8]) -> Result<Vec<u8>> {
	ensure!(key.len() == 16, "an AES-128 key has 16 bytes, got {}", key.len());
	let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(key));
	let mut payload = Vec::with_capacity(ciphertext.len() + tag.len());
	payload.extend_from_slice(ciphertext);
	payload.extend_from_slice(tag);
	cipher
		.decrypt(Nonce::from_slice(iv), payload.as_ref())
		.map_err(|_| anyhow!("AES-GCM authentication of {} bytes failed: wrong key or altered data", ciphertext.len()))
}

/// Encrypts `data`, a whole number of 16-byte blocks, with AES-128-ECB.
pub fn ecb_encrypt(data: &[u8], key: &[u8; 16]) -> Result<Vec<u8>> {
	ensure!(data.len().is_multiple_of(16), "AES-ECB takes whole 16-byte blocks, got {} bytes", data.len());
	let cipher = Aes128::new(&Array::from(*key));
	let mut encrypted = data.to_vec();
	for block in encrypted.chunks_mut(16) {
		cipher.encrypt_block(block.try_into().expect("a 16-byte block"));
	}
	Ok(encrypted)
}

/// Decrypts `data`, a whole number of 16-byte blocks, with AES-128-ECB.
pub fn ecb_decrypt(data: &[u8], key: &[u8; 16]) -> Result<Vec<u8>> {
	ensure!(data.len().is_multiple_of(16), "AES-ECB takes whole 16-byte blocks, got {} bytes", data.len());
	let cipher = Aes128::new(&Array::from(*key));
	let mut decrypted = data.to_vec();
	for block in decrypted.chunks_mut(16) {
		cipher.decrypt_block(block.try_into().expect("a 16-byte block"));
	}
	Ok(decrypted)
}

/// The pairing key of `salt` and `pin`: the first 16 bytes of
/// SHA-256(salt ‖ pin).
pub fn pairing_key(salt: &[u8; 16], pin: &str) -> [u8; 16] {
	let mut hasher = Sha256::new();
	hasher.update(salt);
	hasher.update(pin.as_bytes());
	hasher.finalize()[..16].try_into().expect("SHA-256 has 32 bytes")
}

/// SHA-256 of the concatenation of `parts`.
pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
	let mut hasher = Sha256::new();
	for part in parts {
		hasher.update(part);
	}
	hasher.finalize().into()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn gcm_round_trips_and_rejects_a_changed_byte() {
		let (key, iv) = ([7u8; 16], [3u8; 12]);
		let (ciphertext, tag) = gcm_encrypt(b"control message", &key, &iv).unwrap();
		assert_eq!(gcm_decrypt(&ciphertext, &key, &iv, &tag).unwrap(), b"control message");
		let mut altered = ciphertext.clone();
		altered[0] ^= 1;
		assert!(gcm_decrypt(&altered, &key, &iv, &tag).is_err());
	}

	#[test]
	fn ecb_round_trips_whole_blocks_and_refuses_partial_ones() {
		let key = pairing_key(&[1; 16], "1234");
		let data = [9u8; 48];
		assert_eq!(ecb_decrypt(&ecb_encrypt(&data, &key).unwrap(), &key).unwrap(), data);
		assert!(ecb_encrypt(&data[..20], &key).is_err());
	}
}
