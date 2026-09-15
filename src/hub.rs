//! The single point of contact with QBZ.
//!
//! `openaction` runs one process for every instance of every action, so one
//! D-Bus connection and one state pump serve the whole plugin. Actions never
//! talk to D-Bus directly: they read a [`watch`] snapshot to render, and call
//! the helpers here to act.
//!
//! QBZ is treated as something that comes and goes. The pump follows
//! `NameOwnerChanged` on the MPRIS bus name, so starting the plugin before
//! QBZ, or quitting QBZ mid-session, both settle into `online: false` rather
//! than an error path.

use std::sync::OnceLock;

use tokio::sync::watch;
use zbus::Connection;

use crate::mpris::{Playback, PlayerProxy, TrackInfo, MPRIS_BUS};

/// Everything the actions render from. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct QbzState {
	/// Whether QBZ currently owns its MPRIS bus name.
	pub online: bool,
	pub playback: Playback,
	pub track: TrackInfo,
	/// Best known in-app volume, 0.0-1.0. QBZ never reports this over D-Bus,
	/// so it is tracked by [`crate::volume`] rather than read from the bus.
	pub volume: f64,
	/// Colour taken from the current cover, when the album tint is on.
	///
	/// Arrives after the track does: it needs the artwork downloaded and
	/// scanned, so keys draw once with the plain tile and again in colour.
	pub accent: Option<[u8; 3]>,
	/// Mute is simulated by the plugin: QBZ's own mute is not on the bus.
	///
	/// Set only by the mute key or dial press. Stepping the level down to
	/// zero leaves this `false` — otherwise unmuting later would restore a
	/// level the user never chose.
	pub muted: bool,
}

pub struct Hub {
	/// `None` when the session bus could not be reached. The hub is still
	/// published in that case so actions degrade to "QBZ unavailable"
	/// instead of panicking on a missing singleton.
	conn: Option<Connection>,
	state: watch::Sender<QbzState>,
	/// Serialises read-modify-write volume changes. Two keys pressed in
	/// quick succession must not both read the same starting value.
	pub(crate) volume_lock: tokio::sync::Mutex<()>,
}

static HUB: OnceLock<Hub> = OnceLock::new();

/// The hub, once [`start`] has run.
pub fn hub() -> &'static Hub {
	HUB.get().expect("hub used before start()")
}

/// Publish the hub, then connect to the session bus and spawn the state pump.
pub async fn start() -> zbus::Result<()> {
	let connection = Connection::session().await;
	let (state, _) = watch::channel(QbzState::default());

	let hub = Hub {
		conn: connection.as_ref().ok().cloned(),
		state,
		volume_lock: tokio::sync::Mutex::new(()),
	};
	if HUB.set(hub).is_err() {
		return Ok(());
	}

	let conn = connection?;
	tokio::spawn(async move {
		if let Err(error) = pump(conn).await {
			log::error!("state pump stopped: {error}");
		}
	});

	Ok(())
}

impl Hub {
	/// Subscribe to state snapshots. The current value is available
	/// immediately via `borrow()`.
	pub fn subscribe(&self) -> watch::Receiver<QbzState> {
		self.state.subscribe()
	}

	pub fn state(&self) -> QbzState {
		self.state.borrow().clone()
	}

	/// A player proxy, or an error if QBZ is not running.
	pub async fn player(&self) -> zbus::Result<PlayerProxy<'static>> {
		let conn = self.bus()?;
		if !self.state.borrow().online {
			return Err(zbus::Error::Failure("QBZ is not running".into()));
		}
		PlayerProxy::new(conn).await
	}

	fn bus(&self) -> zbus::Result<&Connection> {
		self.conn
			.as_ref()
			.ok_or_else(|| zbus::Error::Failure("no session bus".into()))
	}

	/// Mutate the published state in place and notify every subscriber.
	///
	/// In place rather than by replacement: the D-Bus pump owns
	/// `online`/`playback`/`track` and the volume shadow owns
	/// `volume`/`muted`, and neither may clobber the other.
	pub(crate) fn update(&self, edit: impl FnOnce(&mut QbzState)) {
		self.state.send_modify(edit);
	}
}

/// Follow QBZ's presence, and mirror its player state while it is there.
async fn pump(conn: Connection) -> zbus::Result<()> {
	use futures_util::StreamExt;

	let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
	// Created before the first presence check, so a QBZ that starts during
	// that check lands in the stream buffer instead of being missed.
	let mut owner_changes = dbus
		.receive_name_owner_changed_with_args(&[(0, MPRIS_BUS)])
		.await?;

	loop {
		// `NameHasOwner` rather than trusting the signal stream: the plugin
		// may have started while QBZ was already running, in which case no
		// NameOwnerChanged will ever arrive for it.
		let online = dbus
			.name_has_owner(MPRIS_BUS.try_into().unwrap())
			.await
			.unwrap_or(false);

		if online {
			// Returns when QBZ goes away, having consumed the name change,
			// or on a connection error. Either way the next iteration
			// re-establishes the truth from `NameHasOwner`.
			if let Err(error) = follow(&conn, &mut owner_changes).await {
				log::warn!("lost contact with QBZ: {error}");
				go_offline();
				// An error that is not a disappearance would spin this loop;
				// a short pause keeps a broken proxy from burning the CPU.
				tokio::time::sleep(std::time::Duration::from_secs(1)).await;
			}
			continue;
		}

		go_offline();
		log::info!("QBZ is not running; waiting for it to appear");
		if owner_changes.next().await.is_none() {
			// The bus connection is gone; nothing left to follow.
			return Ok(());
		}
	}
}

/// Mirror QBZ's player state until it disappears.
async fn follow(
	conn: &Connection,
	owner_changes: &mut zbus::fdo::NameOwnerChangedStream,
) -> zbus::Result<()> {
	use futures_util::StreamExt;

	let player = PlayerProxy::new(conn).await?;

	let mut status_changes = player.receive_playback_status_changed().await;
	let mut metadata_changes = player.receive_metadata_changed().await;

	let playback = Playback::parse(&player.playback_status().await.unwrap_or_default());
	let track = TrackInfo::from_metadata(&player.metadata().await.unwrap_or_default());
	log::info!("QBZ online: {playback:?} - {} / {}", track.artist, track.title);
	hub().update(|s| {
		s.online = true;
		s.playback = playback;
		s.track = track;
	});
	refresh_accent();

	loop {
		tokio::select! {
			Some(change) = status_changes.next() => {
				if let Ok(value) = change.get().await {
					hub().update(|s| s.playback = Playback::parse(&value));
				}
			}
			Some(change) = metadata_changes.next() => {
				if let Ok(value) = change.get().await {
					hub().update(|s| s.track = TrackInfo::from_metadata(&value));
					refresh_accent();
				}
			}
			_ = owner_changes.next() => {
				go_offline();
				return Ok(());
			}
			// Every stream ended, which means the connection is gone. Without
			// this arm `select!` would panic once all branches are disabled.
			else => {
				go_offline();
				return Ok(());
			}
		}
	}
}

/// Clear everything the bus was the source of, keeping the volume shadow.
///
/// The last known volume survives a QBZ restart deliberately: QBZ restores
/// the same persisted level on its way back up, so the shadow is still the
/// best guess available, and it is re-seeded from disk at that point anyway.
fn go_offline() {
	hub().update(|s| {
		s.online = false;
		s.playback = Playback::default();
		s.track = TrackInfo::default();
		s.accent = None;
	});
}

/// Work out the album colour for whatever is playing now.
///
/// Fetching and scanning the cover is off the critical path, so this spawns
/// and lets the keys redraw when the answer lands. Does nothing when the
/// tint is off, so no cover is downloaded for a feature that is not on.
pub fn refresh_accent() {
	if !crate::settings::get().tint_from_artwork {
		hub().update(|s| s.accent = None);
		return;
	}

	let track = hub().state().track;
	let Some(url) = track.art_url.clone() else {
		hub().update(|s| s.accent = None);
		return;
	};

	tokio::spawn(async move {
		let colour = crate::render::album_colour(&url).await;
		if let Some([r, g, b]) = colour {
			log::info!("album colour #{r:02x}{g:02x}{b:02x} for {}", track.title);
		}
		hub().update(|state| {
			// Two tracks in quick succession can finish out of order; only
			// the colour for the track still playing may be applied.
			if state.track.track_id == track.track_id {
				state.accent = colour;
			}
		});
	});
}
