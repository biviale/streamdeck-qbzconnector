//! Play/pause, track skipping and stop — the parts of QBZ that MPRIS gets
//! right, so these are thin wrappers with no local state.

use openaction::*;

use super::NoSettings;
use crate::glyphs::Glyph;
use crate::hub::{hub, QbzState};
use crate::render;

/// Run a player command, flashing the key on failure.
///
/// Every transport call goes through here so that "QBZ is not running" is a
/// visible alert on the key rather than a silent no-op the user has to guess
/// at from a log file.
async fn dispatch<F, Fut>(instance: &Instance, call: F) -> OpenActionResult<()>
where
	F: FnOnce(crate::mpris::PlayerProxy<'static>) -> Fut,
	Fut: std::future::Future<Output = zbus::Result<()>>,
{
	let outcome = match hub().player().await {
		Ok(player) => call(player).await,
		Err(error) => Err(error),
	};

	if let Err(error) = outcome {
		log::warn!("transport command failed: {error}");
		instance.show_alert().await?;
	}
	Ok(())
}

/// Settings for the play/pause key.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlayPauseSettings {
	/// Replace the key's icon with the current cover art.
	///
	/// Off by default: turning it on for existing keys would change what
	/// they look like without being asked.
	pub show_artwork: bool,
}

pub struct PlayPause;

#[async_trait]
impl Action for PlayPause {
	const UUID: ActionUuid = "com.biviale.qbzconnector.playpause";
	type Settings = PlayPauseSettings;

	async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		super::remember_settings(instance, settings);
		render(instance, settings, &hub().state()).await
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
		render(instance, settings, &hub().state()).await
	}

	async fn property_inspector_did_appear(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::publish_defaults(instance, settings).await
	}

	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		dispatch(instance, |p| async move { p.play_pause().await }).await
	}
}

/// What the key should be showing, so an unchanged image is not resent.
///
/// Covers both sources: the cover art this key can show, and the album tint
/// it falls back to.
fn image_key(settings: &PlayPauseSettings, state: &QbzState) -> String {
	if settings.show_artwork && state.online {
		return format!(
			"art|{}|{}",
			state.track.art_url.as_deref().unwrap_or("none"),
			state.playback.is_playing()
		);
	}
	match state.accent {
		Some([r, g, b]) => format!("tint|{r:02x}{g:02x}{b:02x}"),
		None => "plain".to_string(),
	}
}

/// State 1 is "playing" (the key offers a pause), state 0 is everything else.
/// The manifest sets `DisableAutomaticStates` so this is the only thing that
/// moves the key: a press that QBZ ignores must not flip the icon.
///
/// With artwork on, the state image is covered up, so the same information is
/// drawn onto the cover as a badge. The state is still set underneath: it is
/// what shows if the cover cannot be fetched.
async fn render(instance: &Instance, settings: &PlayPauseSettings, state: &QbzState) -> OpenActionResult<()> {
	let playing = state.playback.is_playing();
	instance.set_state(if playing { 1 } else { 0 }).await?;

	let key = image_key(settings, state);
	if !super::image_changed(instance, &key) {
		return Ok(());
	}

	if settings.show_artwork && state.online {
		let badge = if playing {
			render::Badge::Pause
		} else {
			render::Badge::Play
		};
		if let Some(url) = state.track.art_url.as_deref() {
			if let Some(art) = render::cover(url, false, Some(badge)).await {
				return instance.set_image(Some(art), None).await;
			}
		}
	}

	// No cover to show, so fall back to the tint — and if there is no tint
	// either, `paint_states` resets the key to the manifest tile.
	//
	// The guard was already consumed above, so paint both states directly
	// rather than going through `paint_states`, which would check it again.
	let Some(colour) = state.accent else {
		return instance.set_image(None::<String>, None).await;
	};
	instance
		.set_image(crate::glyphs::tinted(Glyph::Play, colour), Some(0))
		.await?;
	instance
		.set_image(crate::glyphs::tinted(Glyph::Pause, colour), Some(1))
		.await
}

pub struct Next;

#[async_trait]
impl Action for Next {
	const UUID: ActionUuid = "com.biviale.qbzconnector.next";
	type Settings = NoSettings;

	async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::paint_states(instance, hub().state().accent, &[(0, Glyph::Next)]).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_drawn(instance);
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		dispatch(instance, |p| async move { p.next().await }).await
	}
}

pub struct Previous;

#[async_trait]
impl Action for Previous {
	const UUID: ActionUuid = "com.biviale.qbzconnector.previous";
	type Settings = NoSettings;

	async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::paint_states(instance, hub().state().accent, &[(0, Glyph::Previous)]).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_drawn(instance);
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		dispatch(instance, |p| async move { p.previous().await }).await
	}
}

pub struct Stop;

#[async_trait]
impl Action for Stop {
	const UUID: ActionUuid = "com.biviale.qbzconnector.stop";
	type Settings = NoSettings;

	async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::paint_states(instance, hub().state().accent, &[(0, Glyph::Stop)]).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_drawn(instance);
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		dispatch(instance, |p| async move { p.stop().await }).await
	}
}

pub async fn refresh(state: &QbzState) {
	for instance in visible_instances(PlayPause::UUID).await {
		let settings = super::instance_settings::<PlayPauseSettings>(&instance);
		if let Err(error) = render(&instance, &settings, state).await {
			log::warn!("failed to render play/pause: {error}");
		}
	}

	for (uuid, glyph) in [
		(Next::UUID, Glyph::Next),
		(Previous::UUID, Glyph::Previous),
		(Stop::UUID, Glyph::Stop),
		(SeekForward::UUID, Glyph::Seek),
		(SeekBackward::UUID, Glyph::SeekBackwards),
	] {
		for instance in visible_instances(uuid).await {
			if let Err(error) = super::paint_states(&instance, state.accent, &[(0, glyph)]).await {
				log::warn!("failed to paint {uuid}: {error}");
			}
		}
	}
}

/// Settings for the two seek keys.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SeekSettings {
	/// Distance travelled per press, in seconds. Always positive; the
	/// direction comes from which action it belongs to.
	pub seconds: f64,
}

impl Default for SeekSettings {
	fn default() -> Self {
		Self { seconds: 10.0 }
	}
}

impl SeekSettings {
	/// Offset in microseconds, which is what MPRIS `Seek` expects.
	fn offset_us(&self, direction: f64) -> i64 {
		(self.seconds.abs().max(1.0) * direction * 1_000_000.0) as i64
	}
}

pub struct SeekForward;

#[async_trait]
impl Action for SeekForward {
	const UUID: ActionUuid = "com.biviale.qbzconnector.seekforward";
	type Settings = SeekSettings;

	async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::paint_states(instance, hub().state().accent, &[(0, Glyph::Seek)]).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_drawn(instance);
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		let offset = settings.offset_us(1.0);
		dispatch(instance, move |p| async move { p.seek(offset).await }).await
	}

	async fn property_inspector_did_appear(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::publish_defaults(instance, settings).await
	}
}

pub struct SeekBackward;

#[async_trait]
impl Action for SeekBackward {
	const UUID: ActionUuid = "com.biviale.qbzconnector.seekbackward";
	type Settings = SeekSettings;

	async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::paint_states(instance, hub().state().accent, &[(0, Glyph::SeekBackwards)]).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_drawn(instance);
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		let offset = settings.offset_us(-1.0);
		dispatch(instance, move |p| async move { p.seek(offset).await }).await
	}

	async fn property_inspector_did_appear(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		super::publish_defaults(instance, settings).await
	}
}
