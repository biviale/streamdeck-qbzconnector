//! The favourite (heart) key.
//!
//! Qobuz has no dislike, so this is a two-way toggle with a third state for
//! "cannot act right now" — no Qobuz session, nothing playing, or a track
//! with no Qobuz identity (local, Plex and Jellyfin sources have none).

use openaction::*;

use super::NoSettings;
use crate::glyphs::Glyph;
use crate::hub::{hub, QbzState};
use crate::qobuz::library;

/// Manifest state indices.
const UNAVAILABLE: u16 = 0;
const NOT_FAVORITE: u16 = 1;
const FAVORITE: u16 = 2;

pub struct Favorite;

#[async_trait]
impl Action for Favorite {
	const UUID: ActionUuid = "com.biviale.qbzconnector.favorite";
	type Settings = NoSettings;

	async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		render(instance, &hub().state()).await
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		super::forget_drawn(instance);
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		let state = hub().state();
		let Some(track_id) = state.track.qobuz_id else {
			// Nothing to favourite: either nothing is playing, or the track
			// is not from the Qobuz catalogue.
			return instance.show_alert().await;
		};

		if !library::is_ready().await {
			log::warn!("favourite pressed with no Qobuz session");
			return instance.show_alert().await;
		}

		match library::toggle(track_id).await {
			Ok(now_favorite) => {
				instance
					.set_state(if now_favorite { FAVORITE } else { NOT_FAVORITE })
					.await?;
				instance.show_ok().await
			}
			Err(error) => {
				log::warn!("could not toggle favourite: {error}");
				instance.show_alert().await
			}
		}
	}

	async fn property_inspector_did_appear(
		&self,
		instance: &Instance,
		_: &Self::Settings,
	) -> OpenActionResult<()> {
		send_status(instance).await
	}

	async fn send_to_plugin(
		&self,
		instance: &Instance,
		_: &Self::Settings,
		payload: &serde_json::Value,
	) -> OpenActionResult<()> {
		match payload.get("action").and_then(|v| v.as_str()) {
			Some("login") => {
				// The browser round trip takes as long as the user does, so
				// it must not block the event loop.
				let context = instance.instance_id.clone();
				tokio::spawn(async move {
					let result = library::login().await;
					if let Err(error) = &result {
						log::warn!("Qobuz sign-in failed: {error}");
					}
					if result.is_ok() {
						persist_identity().await;
					}
					if let Some(instance) = get_instance(context).await {
						let _ = send_status_with_error(&instance, result.err()).await;
					}
					refresh(&hub().state()).await;
				});
				Ok(())
			}
			Some("logout") => {
				library::logout().await;
				persist_identity().await;
				send_status(instance).await?;
				refresh(&hub().state()).await;
				Ok(())
			}
			_ => send_status(instance).await,
		}
	}
}

/// Keep the global settings in step with the session.
///
/// Only the identity goes here, never the token, which lives in the keyring.
/// Routed through the settings cache so it cannot clobber the appearance
/// toggle sharing the same object.
async fn persist_identity() {
	let identity = library::identity().await;
	crate::settings::update(|settings| settings.identity = identity).await;
}

async fn send_status(instance: &Instance) -> OpenActionResult<()> {
	send_status_with_error(instance, None).await
}

async fn send_status_with_error(instance: &Instance, error: Option<String>) -> OpenActionResult<()> {
	let mut status = library::status().await;
	status.error = error;
	instance.send_to_property_inspector(status).await
}

async fn render(instance: &Instance, state: &QbzState) -> OpenActionResult<()> {
	let favorite = match state.track.qobuz_id {
		Some(id) if state.online => library::is_favorite(id).await,
		_ => None,
	};

	let key_state = match favorite {
		Some(true) => FAVORITE,
		Some(false) => NOT_FAVORITE,
		None => UNAVAILABLE,
	};
	instance.set_state(key_state).await?;

	// Only the "not a favourite" state follows the album. The other two are
	// saying something with their colour — crimson means favourited, grey
	// means the key cannot act — and an album must not overrule that.
	super::paint_states(instance, state.accent, &[(NOT_FAVORITE, Glyph::FavoriteOff)]).await
}

pub async fn refresh(state: &QbzState) {
	for instance in visible_instances(Favorite::UUID).await {
		if let Err(error) = render(&instance, state).await {
			log::warn!("failed to render favourite: {error}");
		}
	}
}
