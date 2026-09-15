//! Plugin-wide settings.
//!
//! The server stores one settings object per plugin and `setGlobalSettings`
//! replaces it wholesale. The Qobuz identity and the appearance toggle live in
//! there together and are written by different code at different times, so
//! every write has to carry the other's current value. Hence the cached copy:
//! edit a field, write the whole thing back.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::qobuz::auth::Identity;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GlobalSettings {
	/// Flattened so the Qobuz fields keep the shape they already have on
	/// disk, and an existing sign-in survives this setting being added.
	#[serde(flatten)]
	pub identity: Identity,
	/// Paint every key with a colour taken from the current cover.
	pub tint_from_artwork: bool,
}

static CACHE: Mutex<Option<GlobalSettings>> = Mutex::new(None);

/// The current settings.
pub fn get() -> GlobalSettings {
	CACHE.lock().unwrap().clone().unwrap_or_default()
}

/// Adopt what the server sent, without writing back.
pub fn adopt(value: serde_json::Value) -> GlobalSettings {
	let settings: GlobalSettings = serde_json::from_value(value).unwrap_or_default();
	*CACHE.lock().unwrap() = Some(settings.clone());
	settings
}

/// Edit the settings and persist the whole object.
pub async fn update(edit: impl FnOnce(&mut GlobalSettings)) {
	let settings = {
		let mut guard = CACHE.lock().unwrap();
		let settings = guard.get_or_insert_with(GlobalSettings::default);
		edit(settings);
		settings.clone()
	};

	if let Err(error) = openaction::set_global_settings(&settings).await {
		log::warn!("could not persist global settings: {error}");
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The flattened shape is what is already stored for anyone who signed
	/// in before the tint existed; adding the toggle must not orphan it.
	#[test]
	fn qobuz_fields_stay_at_the_top_level() {
		let settings = GlobalSettings {
			identity: Identity {
				authenticated: true,
				user_id: 42,
				display_name: "someone".into(),
				secret_index: 2,
			},
			tint_from_artwork: true,
		};
		let json = serde_json::to_value(&settings).unwrap();

		assert_eq!(json["authenticated"], serde_json::json!(true));
		assert_eq!(json["userId"], serde_json::json!(42));
		assert_eq!(json["tintFromArtwork"], serde_json::json!(true));
	}

	#[test]
	fn settings_survive_a_round_trip() {
		let settings = GlobalSettings {
			identity: Identity {
				authenticated: true,
				user_id: 7,
				display_name: "x".into(),
				secret_index: 1,
			},
			tint_from_artwork: true,
		};
		let back: GlobalSettings = serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
		assert!(back.identity.authenticated);
		assert_eq!(back.identity.user_id, 7);
		assert_eq!(back.identity.secret_index, 1);
		assert!(back.tint_from_artwork);
	}

	/// Settings written before the toggle existed must still load.
	#[test]
	fn an_older_object_without_the_toggle_still_loads() {
		let stored = serde_json::json!({
			"authenticated": true,
			"userId": 12345678,
			"displayName": "someone",
			"secretIndex": 0
		});
		let settings: GlobalSettings = serde_json::from_value(stored).unwrap();
		assert!(settings.identity.authenticated);
		assert!(!settings.tint_from_artwork);
	}

	#[test]
	fn an_empty_object_is_the_default() {
		let settings: GlobalSettings = serde_json::from_value(serde_json::json!({})).unwrap();
		assert!(!settings.identity.authenticated);
		assert!(!settings.tint_from_artwork);
	}
}
