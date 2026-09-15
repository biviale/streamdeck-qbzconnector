//! Reading and writing the account's favourite tracks.

use std::collections::HashSet;

use super::auth::Session;
use super::{bundle::Bundle, sign, API_BASE};

/// Signature for `favorite/getUserFavorites`.
///
/// The parameter string is **empty**, even though `type`, `limit` and
/// `offset` are sent in the query. Every other signed endpoint folds its
/// query parameters into the preimage; this one does not, and signing it the
/// usual way is rejected.
fn list_sig(secret: &str, timestamp: u64) -> String {
	sign::request_sig("favoritegetUserFavorites", &[], timestamp, secret)
}

/// Send a signed request, trying each bundle secret until one is accepted.
///
/// Returns the response and the index of the secret that worked.
async fn try_secrets<F>(
	bundle: &Bundle,
	preferred: usize,
	build: F,
) -> Result<(reqwest::Response, usize), String>
where
	F: Fn(usize) -> reqwest::RequestBuilder,
{
	let mut last_error = "no secrets available".to_string();

	for index in secret_order(bundle.secrets.len(), preferred) {
		let response = build(index)
			.send()
			.await
			.map_err(|error| format!("request failed: {error}"))?;

		if response.status().is_success() {
			return Ok((response, index));
		}
		last_error = format!("refused ({})", response.status());
	}

	Err(last_error)
}

/// Page size for the initial favourites sweep. Qobuz caps this well above
/// what a single request needs, and fewer round trips means the heart is
/// correct sooner after the plugin starts.
const PAGE_SIZE: u32 = 500;

/// Stop after this many pages, so a very large library cannot stall startup
/// indefinitely.
const MAX_PAGES: u32 = 20;

/// Add the track to the account's favourites.
pub async fn add(session: &Session, bundle: &Bundle, track_id: u64) -> Result<usize, String> {
	call(session, bundle, "favorite/create", track_id).await
}

/// Remove it again.
pub async fn remove(session: &Session, bundle: &Bundle, track_id: u64) -> Result<usize, String> {
	call(session, bundle, "favorite/delete", track_id).await
}

/// Issue a signed favourite call, returning the secret index that worked.
///
/// The bundle ships several candidate secrets and does not say which is live.
/// A wrong one fails the signature check, indistinguishable from any other
/// 4xx, so they are tried in turn starting from the last one that worked.
async fn call(session: &Session, bundle: &Bundle, path: &str, track_id: u64) -> Result<usize, String> {
	let method = path.replace('/', "");
	let params = vec![("track_ids", track_id.to_string())];
	let client = super::http_client().map_err(|e| e.to_string())?;

	// Unlike the listing endpoint, these fold their query parameters into the
	// signature preimage, which is what `request_sig` does by default.
	try_secrets(bundle, session.identity.secret_index, |index| {
		let ts = sign::timestamp();
		let sig = sign::request_sig(&method, &params, ts, &bundle.secrets[index]);

		let mut query: Vec<(&str, String)> = params.clone();
		query.push(("request_ts", ts.to_string()));
		query.push(("request_sig", sig));

		client
			.get(format!("{API_BASE}/{path}"))
			.header("X-App-Id", &bundle.app_id)
			.header("X-User-Auth-Token", &session.token)
			.query(&query)
	})
	.await
	.map(|(_, index)| index)
	.map_err(|error| format!("{path} {error}"))
}

/// Secret indices to try, most likely first.
fn secret_order(count: usize, preferred: usize) -> Vec<usize> {
	let mut order: Vec<usize> = (0..count).collect();
	if preferred < count {
		order.remove(preferred);
		order.insert(0, preferred);
	}
	order
}

/// Every favourited track id on the account.
///
/// Fetched once at sign-in so the heart is right before the user presses
/// anything; kept current afterwards by the toggle itself. This mirrors what
/// QBZ does with its own `favorites_cache.db`.
pub async fn track_ids(session: &Session, bundle: &Bundle) -> Result<HashSet<u64>, String> {
	let client = super::http_client().map_err(|e| e.to_string())?;
	let mut ids = HashSet::new();
	let mut secret = session.identity.secret_index;

	for page in 0..MAX_PAGES {
		let offset = page * PAGE_SIZE;
		let (response, used) = match try_secrets(bundle, secret, |index| {
			let ts = sign::timestamp();
			let sig = list_sig(&bundle.secrets[index], ts);
			client
				.get(format!("{API_BASE}/favorite/getUserFavorites"))
				.header("X-App-Id", &bundle.app_id)
				.header("X-User-Auth-Token", &session.token)
				.query(&[
					("type", "tracks".to_string()),
					("limit", PAGE_SIZE.to_string()),
					("offset", offset.to_string()),
					("request_ts", ts.to_string()),
					("request_sig", sig),
				])
		})
		.await
		{
			Ok(pair) => pair,
			Err(error) => return Err(error),
		};
		// Stay on whichever secret just worked for the remaining pages.
		secret = used;

		let json: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
		let items = json
			.get("tracks")
			.and_then(|t| t.get("items"))
			.and_then(|i| i.as_array())
			.cloned()
			.unwrap_or_default();

		let before = ids.len();
		for item in &items {
			if let Some(id) = item.get("id").and_then(|v| v.as_u64()) {
				ids.insert(id);
			}
		}

		// A short page is the last page. Also stop if a full page added
		// nothing new, which means the API is repeating itself.
		if (items.len() as u32) < PAGE_SIZE || ids.len() == before {
			break;
		}
	}

	Ok(ids)
}

#[cfg(test)]
mod tests {
	use super::secret_order;

	#[test]
	fn the_last_known_good_secret_is_tried_first() {
		assert_eq!(secret_order(3, 2), vec![2, 0, 1]);
		assert_eq!(secret_order(3, 0), vec![0, 1, 2]);
	}

	#[test]
	fn an_out_of_range_preference_is_ignored() {
		assert_eq!(secret_order(2, 9), vec![0, 1]);
	}

	/// The listing endpoint signs an empty parameter string even though it
	/// sends `type`, `limit` and `offset`. Signed like every other endpoint it
	/// is rejected, the sweep comes back empty, and every heart draws as "not a
	/// favourite".
	#[test]
	fn the_listing_signature_ignores_the_query_parameters() {
		let expected = super::sign::request_sig("favoritegetUserFavorites", &[], 1000, "s");
		assert_eq!(super::list_sig("s", 1000), expected);

		let with_params = super::sign::request_sig(
			"favoritegetUserFavorites",
			&[("type", "tracks".to_string())],
			1000,
			"s",
		);
		assert_ne!(super::list_sig("s", 1000), with_params);
	}
}
