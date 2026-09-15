//! Just enough of the Qobuz API to favourite a track.
//!
//! Qobuz has no concept of a dislike: a track is either in your favourites or
//! it is not, so the favourite key is a toggle.
//!
//! Portions of the request signing and OAuth flow are derived from QBZ
//! (<https://github.com/vicrodh/qbz>), MIT licensed — see LICENSES/qbz-MIT.txt.

pub mod auth;
pub mod bundle;
pub mod favorites;
pub mod library;
pub mod sign;

/// Qobuz JSON API root.
pub const API_BASE: &str = "https://www.qobuz.com/api.json/0.2";

/// Shared HTTP client settings.
pub fn http_client() -> reqwest::Result<reqwest::Client> {
	reqwest::Client::builder()
		.timeout(std::time::Duration::from_secs(20))
		.build()
}
