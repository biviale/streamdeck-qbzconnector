//! The "what's playing" key: cover art with the track under it.
//!
//! A display, not a control: pressing it does nothing on purpose. Raising
//! QBZ's window was the obvious thing to put here, maybe in a future update.

use openaction::*;
use serde::{Deserialize, Serialize};

use crate::glyphs::Glyph;
use crate::hub::{hub, QbzState};
use crate::render;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
	pub show_artwork: bool,
	/// One of `title`, `artist`, `both`, `none`.
	pub title_mode: String,
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			show_artwork: true,
			title_mode: "both".to_string(),
		}
	}
}

/// Identity of what a key should be showing, so a redraw can be skipped.
fn image_key(settings: &Settings, state: &QbzState) -> String {
	if settings.show_artwork && state.online {
		return format!(
			"art|{}|{}",
			state.track.art_url.as_deref().unwrap_or("none"),
			settings.title_mode
		);
	}
	match state.accent {
		Some([r, g, b]) => format!("tint|{r:02x}{g:02x}{b:02x}"),
		None => "plain".to_string(),
	}
}

pub struct NowPlaying;

#[async_trait]
impl Action for NowPlaying {
	const UUID: ActionUuid = "com.biviale.qbzconnector.nowplaying";
	type Settings = Settings;

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
}

/// Text to put under the cover.
fn title(settings: &Settings, state: &QbzState) -> Option<String> {
	if !state.online {
		return Some("QBZ\noffline".to_string());
	}
	let track = &state.track;
	if track.title.is_empty() && track.artist.is_empty() {
		return None;
	}
	match settings.title_mode.as_str() {
		"none" => None,
		"title" => Some(track.title.clone()),
		"artist" => Some(track.artist.clone()),
		// A key is 72 px wide, so two short lines read better than one long
		// one the server will clip.
		_ => Some(format!("{}\n{}", track.title, track.artist)),
	}
}

async fn render(instance: &Instance, settings: &Settings, state: &QbzState) -> OpenActionResult<()> {
	// The title is a short string; it can be resent freely.
	instance.set_title(title(settings, state), None).await?;

	let key = image_key(settings, state);
	if !super::image_changed(instance, &key) {
		return Ok(());
	}

	if settings.show_artwork && state.online {
		if let Some(url) = state.track.art_url.as_deref() {
			// The scrim is only worth its cost when text sits on top; no
			// badge, because this key is a display, not a transport control.
			if let Some(art) = render::cover(url, settings.title_mode != "none", None).await {
				return instance.set_image(Some(art), None).await;
			}
		}
	}

	// No cover to show, so fall back to the album tint. With no tint either,
	// an absent image resets the key to the manifest tile.
	match state.accent {
		Some(colour) => {
			instance
				.set_image(crate::glyphs::tinted(Glyph::NowPlaying, colour), Some(0))
				.await
		}
		None => instance.set_image(None::<String>, None).await,
	}
}

pub async fn refresh(state: &QbzState) {
	for instance in visible_instances(NowPlaying::UUID).await {
		let settings = super::instance_settings::<Settings>(&instance);
		if let Err(error) = render(&instance, &settings, state).await {
			log::warn!("failed to render now playing: {error}");
		}
	}
}
