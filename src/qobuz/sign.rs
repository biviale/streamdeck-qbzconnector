//! Qobuz request signing.
//!
//! Protected endpoints want `request_ts` and `request_sig` alongside the
//! normal query parameters. The signature is
//! `MD5(method + params + timestamp + secret)`, where `method` is the
//! endpoint path with its slashes removed (`/favorite/create` becomes
//! `favoritecreate`) and `params` is the query pairs sorted by key and
//! concatenated without separators.
//!
//! Derived from QBZ's `crates/qbz-qobuz/src/auth.rs` (MIT).

use md5::{Digest, Md5};

/// Seconds since the Unix epoch, as the API expects.
pub fn timestamp() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_secs())
		.unwrap_or(0)
}

/// Signature for a call to `method` with `params`.
pub fn request_sig(method: &str, params: &[(&str, String)], timestamp: u64, secret: &str) -> String {
	let mut sorted: Vec<_> = params.iter().collect();
	sorted.sort_by(|a, b| a.0.cmp(b.0));
	let joined: String = sorted.iter().map(|(k, v)| format!("{k}{v}")).collect();

	let mut hasher = Md5::new();
	hasher.update(format!("{method}{joined}{timestamp}{secret}").as_bytes());
	hex(&hasher.finalize())
}

/// Lowercase hex, which is the form Qobuz expects.
fn hex(bytes: &[u8]) -> String {
	use std::fmt::Write as _;

	bytes
		.iter()
		.fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
			let _ = write!(out, "{byte:02x}");
			out
		})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The preimage assembly is the whole algorithm, so it is asserted
	/// directly rather than against a golden hash.
	#[test]
	fn params_are_sorted_by_key_and_concatenated_without_separators() {
		let got = request_sig(
			"favoritecreate",
			&[("track_ids", "100".to_string()), ("extra", "1".to_string())],
			1001,
			"s",
		);

		let mut hasher = Md5::new();
		hasher.update(b"favoritecreateextra1track_ids1001001s");
		assert_eq!(got, hex(&hasher.finalize()));
	}

	#[test]
	fn signature_is_lowercase_hex_of_the_right_length() {
		let sig = request_sig("favoritedelete", &[], 0, "secret");
		assert_eq!(sig.len(), 32);
		assert!(sig
			.chars()
			.all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
	}
}
