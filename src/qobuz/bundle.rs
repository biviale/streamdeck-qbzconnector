//! Application-level Qobuz credentials.
//!
//! Every Qobuz API call needs an `app_id` and, for signed endpoints, one of
//! several `secrets`. Both live inside the Qobuz web player's JavaScript
//! bundle and rotate when Qobuz redeploys it.
//!
//! This plugin does **not** scrape that bundle. QBZ already does and caches
//! the result in `~/.cache/qbz/bundle_tokens.json`, and the plugin is useless
//! without QBZ anyway, so it reads that cache instead of keeping a second copy
//! of the most breakage-prone logic in the ecosystem. If the cache is missing,
//! open QBZ and sign in once.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Bundle {
	pub app_id: String,
	/// Candidate signing secrets. Only one is valid at a time and the bundle
	/// does not say which, so callers try them in turn.
	pub secrets: Vec<String>,
	/// Needed for the OAuth code exchange. Absent on older bundles.
	#[serde(default)]
	pub private_key: Option<String>,
}

fn cache_path() -> Option<PathBuf> {
	Some(dirs::cache_dir()?.join("qbz").join("bundle_tokens.json"))
}

/// Read the credentials QBZ extracted.
pub fn load() -> Result<Bundle, String> {
	let path = cache_path().ok_or("no cache directory")?;
	let text = std::fs::read_to_string(&path).map_err(|error| {
		format!(
			"cannot read {} ({error}). Open QBZ and sign in once so it \
			 populates this file.",
			path.display()
		)
	})?;
	let bundle: Bundle =
		serde_json::from_str(&text).map_err(|error| format!("malformed bundle cache: {error}"))?;

	if bundle.app_id.is_empty() || bundle.secrets.is_empty() {
		return Err("bundle cache has no app id or secrets".to_string());
	}
	Ok(bundle)
}
