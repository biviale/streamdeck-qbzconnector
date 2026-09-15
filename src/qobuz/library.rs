//! The plugin's Qobuz session and its cache of favourited tracks.
//!
//! One session serves every favourite key, and the set of favourited track
//! ids is held in memory so a heart can be drawn without a network round
//! trip on every track change.

use std::collections::HashSet;

use tokio::sync::RwLock;

use super::auth::{self, Identity, Session};
use super::bundle::{self, Bundle};
use super::favorites;

struct Ready {
	session: Session,
	bundle: Bundle,
	favorites: HashSet<u64>,
}

static STATE: RwLock<Option<Ready>> = RwLock::const_new(None);

/// What the property inspector and the key need to know.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
	pub authenticated: bool,
	pub display_name: String,
	pub favorite_count: usize,
	/// Populated when something is wrong that the user can act on.
	pub error: Option<String>,
}

pub async fn status() -> Status {
	match STATE.read().await.as_ref() {
		Some(ready) => Status {
			authenticated: true,
			display_name: ready.session.identity.display_name.clone(),
			favorite_count: ready.favorites.len(),
			error: None,
		},
		None => Status::default(),
	}
}

/// Whether a session is usable right now.
pub async fn is_ready() -> bool {
	STATE.read().await.is_some()
}

/// Bring a stored session back after a restart.
///
/// Called with the identity from the global settings; the token itself comes
/// from the keyring.
pub async fn restore(identity: &Identity) {
	let Some(session) = auth::restore(identity).await else {
		return;
	};
	let bundle = match bundle::load() {
		Ok(bundle) => bundle,
		Err(error) => {
			log::warn!("Qobuz credentials unavailable: {error}");
			return;
		}
	};

	log::info!("restoring Qobuz session for {}", identity.display_name);
	activate(session, bundle).await;
}

/// Run the browser sign-in and adopt the resulting session.
pub async fn login() -> Result<Identity, String> {
	let session = auth::login().await?;
	let bundle = bundle::load()?;
	let identity = session.identity.clone();
	activate(session, bundle).await;
	Ok(identity)
}

/// Forget the session, locally and in the keyring.
pub async fn logout() {
	*STATE.write().await = None;
	auth::store::clear().await;
	log::info!("Qobuz session cleared");
}

/// Install a session and warm the favourites cache.
async fn activate(session: Session, bundle: Bundle) {
	// The sweep is best-effort: a session that cannot list favourites can
	// still toggle them, it just starts with an empty heart.
	let favorites = match favorites::track_ids(&session, &bundle).await {
		Ok(ids) => {
			log::info!("{} favourite tracks known", ids.len());
			ids
		}
		Err(error) => {
			log::warn!("could not load favourites: {error}");
			HashSet::new()
		}
	};

	*STATE.write().await = Some(Ready {
		session,
		bundle,
		favorites,
	});
}

/// Whether a track is favourited, or `None` if there is no session.
pub async fn is_favorite(track_id: u64) -> Option<bool> {
	let guard = STATE.read().await;
	guard.as_ref().map(|ready| ready.favorites.contains(&track_id))
}

/// Flip a track's favourite state. Returns the new state.
pub async fn toggle(track_id: u64) -> Result<bool, String> {
	// The call is made outside the write lock: it is a network round trip,
	// and holding the lock across it would block every key redraw.
	let (session, bundle, was_favorite) = {
		let guard = STATE.read().await;
		let ready = guard.as_ref().ok_or("not signed in to Qobuz")?;
		(
			ready.session.clone(),
			ready.bundle.clone(),
			ready.favorites.contains(&track_id),
		)
	};

	let used_secret = if was_favorite {
		favorites::remove(&session, &bundle, track_id).await?
	} else {
		favorites::add(&session, &bundle, track_id).await?
	};

	let mut guard = STATE.write().await;
	if let Some(ready) = guard.as_mut() {
		if was_favorite {
			ready.favorites.remove(&track_id);
		} else {
			ready.favorites.insert(track_id);
		}
		// Remember which secret signed successfully so the next call starts
		// with it instead of walking the list again.
		ready.session.identity.secret_index = used_secret;
	}
	Ok(!was_favorite)
}

/// The identity to persist in the global settings.
pub async fn identity() -> Identity {
	STATE
		.read()
		.await
		.as_ref()
		.map(|ready| ready.session.identity.clone())
		.unwrap_or_default()
}
