//! Volume keys and the encoder.
//!
//! All of these read and write [`crate::volume`], never the bus directly, so
//! the shadow level stays the single source of truth for what is displayed.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use openaction::*;
use serde::{Deserialize, Serialize};

use super::NoSettings;
use crate::glyphs::Glyph;
use crate::hub::{hub, QbzState};
use crate::volume::{self, StepSettings};

/// How long a key must be held to count as a resynchronisation request.
const LONG_PRESS: Duration = Duration::from_millis(700);

/// Key-down timestamps, so `key_up` can tell a tap from a hold.
///
/// OpenAction has no long-press event, but it delivers both edges, which is
/// enough to measure the gap.
static PRESSED_AT: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn mark_pressed(instance: &Instance) {
	let mut guard = PRESSED_AT.lock().unwrap();
	guard
		.get_or_insert_with(HashMap::new)
		.insert(instance.instance_id.clone(), Instant::now());
}

/// Whether the press that just ended was a long one.
fn was_long_press(instance: &Instance) -> bool {
	let mut guard = PRESSED_AT.lock().unwrap();
	guard
		.as_mut()
		.and_then(|map| map.remove(&instance.instance_id))
		.map(|at| at.elapsed() >= LONG_PRESS)
		.unwrap_or(false)
}

/// Report the outcome of a volume change on the key.
async fn settle(instance: &Instance, outcome: zbus::Result<f64>) -> OpenActionResult<()> {
	if let Err(error) = outcome {
		log::warn!("volume change failed: {error}");
		instance.show_alert().await?;
	}
	Ok(())
}

/// Shared handling for the up and down keys.
async fn nudge(instance: &Instance, settings: &StepSettings, direction: f64) -> OpenActionResult<()> {
	if was_long_press(instance) {
		// Recover from drift caused by QBZ's own keyboard shortcuts, which
		// change the level without persisting it anywhere we can see.
		volume::resync_from_disk().await;
		return instance.show_ok().await;
	}
	settle(instance, volume::step(settings, direction).await).await
}

pub struct VolumeUp;

#[async_trait]
impl Action for VolumeUp {
	const UUID: ActionUuid = "com.biviale.qbzconnector.volumeup";
	type Settings = StepSettings;

	async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		super::remember_settings(instance, settings);
		render_key(instance, settings, &hub().state()).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_settings(instance);
		super::forget_drawn(instance);
		Ok(())
	}

	async fn did_receive_settings(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::remember_settings(instance, settings);
		render_key(instance, settings, &hub().state()).await
	}

	async fn key_down(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		mark_pressed(instance);
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		nudge(instance, settings, 1.0).await
	}

	async fn property_inspector_did_appear(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::publish_defaults(instance, settings).await
	}
}

pub struct VolumeDown;

#[async_trait]
impl Action for VolumeDown {
	const UUID: ActionUuid = "com.biviale.qbzconnector.volumedown";
	type Settings = StepSettings;

	async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		super::remember_settings(instance, settings);
		render_key(instance, settings, &hub().state()).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_settings(instance);
		super::forget_drawn(instance);
		Ok(())
	}

	async fn did_receive_settings(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::remember_settings(instance, settings);
		render_key(instance, settings, &hub().state()).await
	}

	async fn key_down(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		mark_pressed(instance);
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		nudge(instance, settings, -1.0).await
	}

	async fn property_inspector_did_appear(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::publish_defaults(instance, settings).await
	}
}

/// Settings for the "set to a fixed level" key.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TargetSettings {
	/// Target level, 0-100.
	pub target_percent: f64,
	pub show_percent: bool,
}

impl Default for TargetSettings {
	fn default() -> Self {
		Self {
			target_percent: 30.0,
			show_percent: true,
		}
	}
}

pub struct VolumeSet;

#[async_trait]
impl Action for VolumeSet {
	const UUID: ActionUuid = "com.biviale.qbzconnector.volumeset";
	type Settings = TargetSettings;

	async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		super::remember_settings(instance, settings);
		render_target(instance, settings, &hub().state()).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_settings(instance);
		super::forget_drawn(instance);
		Ok(())
	}

	async fn did_receive_settings(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::remember_settings(instance, settings);
		render_target(instance, settings, &hub().state()).await
	}

	async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		// An absolute write is the one operation that cannot drift: after it
		// the shadow is exactly what QBZ was told.
		settle(instance, volume::set(settings.target_percent / 100.0).await).await
	}

	async fn property_inspector_did_appear(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::publish_defaults(instance, settings).await
	}
}

pub struct Mute;

#[async_trait]
impl Action for Mute {
	const UUID: ActionUuid = "com.biviale.qbzconnector.mute";
	type Settings = NoSettings;

	async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		render_mute(instance, &hub().state()).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_drawn(instance);
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		if let Err(error) = volume::toggle_mute().await {
			log::warn!("mute toggle failed: {error}");
			instance.show_alert().await?;
		}
		Ok(())
	}
}

/// The Stream Deck+ dial.
///
/// Untested: the development machine has a 15-key MK.2 with no dials.
pub struct VolumeDial;

#[async_trait]
impl Action for VolumeDial {
	const UUID: ActionUuid = "com.biviale.qbzconnector.volume";
	type Settings = StepSettings;

	async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		super::remember_settings(instance, settings);
		instance.set_feedback_layout("$B1".to_string()).await?;
		render_dial(instance, settings, &hub().state()).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_settings(instance);
		super::forget_drawn(instance);
		Ok(())
	}

	async fn did_receive_settings(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::remember_settings(instance, settings);
		render_dial(instance, settings, &hub().state()).await
	}

	async fn dial_rotate(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
		ticks: i16,
		_pressed: bool,
	) -> OpenActionResult<()> {
		if ticks == 0 {
			return Ok(());
		}
		// One step per detent, applied in sequence so a perceptual curve
		// compounds the way turning the dial slowly would.
		let direction = if ticks > 0 { 1.0 } else { -1.0 };
		let mut outcome = Ok(hub().state().volume);
		for _ in 0..ticks.unsigned_abs().min(32) {
			outcome = volume::step(settings, direction).await;
			if outcome.is_err() {
				break;
			}
		}
		settle(instance, outcome).await
	}

	async fn dial_down(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		if let Err(error) = volume::toggle_mute().await {
			log::warn!("mute toggle failed: {error}");
			instance.show_alert().await?;
		}
		Ok(())
	}

	async fn touch_tap(
		&self,
		instance: &Instance,
		_: &Self::Settings,
		_position: (u16, u16),
		hold: bool,
	) -> OpenActionResult<()> {
		let outcome = if hold {
			volume::set(0.0).await
		} else {
			volume::toggle_mute().await.map(|_| hub().state().volume)
		};
		settle(instance, outcome).await
	}

	async fn property_inspector_did_appear(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::publish_defaults(instance, settings).await
	}
}

/// Level as a rounded percentage string, or a mute marker.
fn label(state: &QbzState) -> String {
	if !state.online {
		return "—".to_string();
	}
	if state.muted {
		return "muted".to_string();
	}
	format!("{} %", (state.volume * 100.0).round() as i64)
}

async fn render_key(instance: &Instance, settings: &StepSettings, state: &QbzState) -> OpenActionResult<()> {
	let title = settings.show_percent.then(|| label(state));
	instance.set_title(title, None).await?;
	super::paint_states(instance, state.accent, &[(0, glyph_for(instance))]).await
}

/// Which speaker glyph a level key carries, from the action it belongs to.
fn glyph_for(instance: &Instance) -> Glyph {
	match instance.action_uuid.as_str() {
		VolumeDown::UUID => Glyph::VolumeDown,
		_ => Glyph::VolumeUp,
	}
}

async fn render_target(
	instance: &Instance,
	settings: &TargetSettings,
	state: &QbzState,
) -> OpenActionResult<()> {
	// This key shows its target, not the live level: its whole purpose is to
	// be a labelled preset.
	let title = settings
		.show_percent
		.then(|| format!("{} %", settings.target_percent.round() as i64));
	instance.set_title(title, None).await?;
	super::paint_states(instance, state.accent, &[(0, Glyph::VolumeSet)]).await
}

async fn render_mute(instance: &Instance, state: &QbzState) -> OpenActionResult<()> {
	instance.set_state(if state.muted { 1 } else { 0 }).await?;
	// The muted glyph stays on the tint like any other: the state index
	// already says which one the key is in.
	super::paint_states(instance, state.accent, &[(0, Glyph::Volume), (1, Glyph::Muted)]).await
}

/// `$B1` carries a title, an icon, a value and a bar indicator.
async fn render_dial(
	instance: &Instance,
	_settings: &StepSettings,
	state: &QbzState,
) -> OpenActionResult<()> {
	let percent = (state.volume * 100.0).round() as i64;
	instance
		.set_feedback(&serde_json::json!({
			"title": "QBZ",
			"value": label(state),
			"indicator": if state.muted { 0 } else { percent },
			"icon": if state.muted { "icons/muted" } else { "icons/volume" },
		}))
		.await
}

pub async fn refresh(state: &QbzState) {
	for instance in visible_instances(VolumeUp::UUID).await {
		let settings = super::instance_settings::<StepSettings>(&instance);
		if let Err(error) = render_key(&instance, &settings, state).await {
			log::warn!("failed to render volume up: {error}");
		}
	}
	for instance in visible_instances(VolumeDown::UUID).await {
		let settings = super::instance_settings::<StepSettings>(&instance);
		if let Err(error) = render_key(&instance, &settings, state).await {
			log::warn!("failed to render volume down: {error}");
		}
	}
	for instance in visible_instances(VolumeSet::UUID).await {
		let settings = super::instance_settings::<TargetSettings>(&instance);
		if let Err(error) = render_target(&instance, &settings, state).await {
			log::warn!("failed to render volume preset: {error}");
		}
	}
	for instance in visible_instances(Mute::UUID).await {
		if let Err(error) = render_mute(&instance, state).await {
			log::warn!("failed to render mute: {error}");
		}
	}
	for instance in visible_instances(VolumeDial::UUID).await {
		let settings = super::instance_settings::<StepSettings>(&instance);
		if let Err(error) = render_dial(&instance, &settings, state).await {
			log::warn!("failed to render volume dial: {error}");
		}
	}
}
