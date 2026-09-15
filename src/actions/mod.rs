//! Action handlers.
//!
//! Every action follows the same shape: input handlers act on QBZ, and
//! rendering is driven centrally from [`refresh_all`] whenever the hub
//! publishes a new snapshot. Actions never poll — they are told what changed.

pub mod favorite;
pub mod nowplaying;
pub mod transport;
pub mod volume;

use std::collections::HashMap;
use std::sync::Mutex;

use openaction::Instance;
use serde::{Deserialize, Serialize};

use crate::hub::QbzState;

/// Settings placeholder for actions that have none.
///
/// A braced struct rather than `()`: the server sends `{}` for an action with
/// no configured settings, and `()` only deserializes from `null`, which
/// would log an error on every event.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NoSettings {}

/// Last known settings per instance.
///
/// `openaction` keeps an instance's settings private, and only hands them to
/// event callbacks. Re-rendering happens outside any callback — it is driven
/// by QBZ changing state — so the settings needed to draw a key are cached
/// here as they arrive.
static SETTINGS: Mutex<Option<HashMap<String, serde_json::Value>>> = Mutex::new(None);

/// Record an instance's settings. Call from `will_appear` and
/// `did_receive_settings`.
pub fn remember_settings(instance: &Instance, settings: &impl Serialize) {
	let Ok(value) = serde_json::to_value(settings) else {
		return;
	};
	SETTINGS
		.lock()
		.unwrap()
		.get_or_insert_with(HashMap::new)
		.insert(instance.instance_id.clone(), value);
}

/// Drop an instance's settings. Call from `will_disappear`.
pub fn forget_settings(instance: &Instance) {
	if let Some(map) = SETTINGS.lock().unwrap().as_mut() {
		map.remove(&instance.instance_id);
	}
}

/// The cached settings for an instance, or the type's default.
pub fn instance_settings<T: serde::de::DeserializeOwned + Default>(instance: &Instance) -> T {
	let guard = SETTINGS.lock().unwrap();
	guard
		.as_ref()
		.and_then(|map| map.get(&instance.instance_id))
		.and_then(|value| serde_json::from_value(value.clone()).ok())
		.unwrap_or_default()
}

/// What each instance last had drawn on it.
///
/// Any state change re-renders everything, including every volume tick, and
/// a cover is tens of kilobytes of base64 on the wire.
static DRAWN: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Whether an instance's image needs sending, remembering the answer.
///
/// `key` identifies what the key should be showing: if it matches what was
/// sent last time, the caller can skip the work entirely.
pub fn image_changed(instance: &Instance, key: &str) -> bool {
	let mut guard = DRAWN.lock().unwrap();
	let map = guard.get_or_insert_with(HashMap::new);
	if map.get(&instance.instance_id).map(String::as_str) == Some(key) {
		return false;
	}
	map.insert(instance.instance_id.clone(), key.to_string());
	true
}

/// Forget what an instance had drawn. Call from `will_disappear`.
pub fn forget_drawn(instance: &Instance) {
	if let Some(map) = DRAWN.lock().unwrap().as_mut() {
		map.remove(&instance.instance_id);
	}
}

/// Paint an instance's states, tinted with the album colour when there is one.
///
/// `states` pairs each state index with the glyph it should carry. Every
/// state is painted, not just the active one, so switching state — a track
/// starting, a mute toggling — never flashes the untinted tile first.
pub async fn paint_states(
	instance: &Instance,
	accent: Option<[u8; 3]>,
	states: &[(u16, crate::glyphs::Glyph)],
) -> openaction::OpenActionResult<()> {
	let key = match accent {
		Some([r, g, b]) => format!("tint|{r:02x}{g:02x}{b:02x}"),
		None => "plain".to_string(),
	};
	if !image_changed(instance, &key) {
		return Ok(());
	}

	let Some(colour) = accent else {
		// An absent image resets every state to the manifest's own tile,
		// which is exactly the right thing when the tint is off.
		return instance.set_image(None::<String>, None).await;
	};

	for (index, glyph) in states {
		instance
			.set_image(crate::glyphs::tinted(*glyph, colour), Some(*index))
			.await?;
	}
	Ok(())
}

/// Write an action's effective settings back when its property inspector
/// opens.
///
/// Serde fills in defaults on the way in, so an unconfigured key already
/// behaves as documented; without this the inspector would show empty fields
/// next to that behaviour.
pub async fn publish_defaults(
	instance: &Instance,
	settings: &impl Serialize,
) -> openaction::OpenActionResult<()> {
	instance.set_settings(settings).await
}

/// Redraw every visible instance of every action for a new state.
pub async fn refresh_all(state: &QbzState) {
	transport::refresh(state).await;
	volume::refresh(state).await;
	nowplaying::refresh(state).await;
	favorite::refresh(state).await;
}
