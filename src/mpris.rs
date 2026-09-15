//! D-Bus surface of the QBZ desktop client.
//!
//! QBZ registers `org.mpris.MediaPlayer2.com.blitzfc.qbz` unconditionally.
//! Its private `com.blitzfc.qbz` name is not proxied here: it offers only
//! `Present`, which no compositor honours under Wayland, and `OpenUrl`.
//!
//! Two upstream quirks drive the design of everything above this module:
//!
//!   * `Volume` is **write-only in practice**. The setter forwards to the
//!     engine, but `qbz-qt` never feeds the property back, so the getter
//!     answers its initial `1.0` forever and never emits `PropertiesChanged`.
//!     [`crate::volume`] tracks the real value instead.
//!   * `Shuffle`, `LoopStatus` and every `Can*` predicate are hardcoded
//!     constants with no-op setters. They are deliberately not proxied.

use std::collections::HashMap;

use zbus::zvariant::OwnedValue;

/// Well-known name QBZ publishes its MPRIS player on.
///
/// The proxies below repeat these literals because the attribute macro needs
/// them at expansion time; the private single-instance name is
/// `com.blitzfc.qbz`.
pub const MPRIS_BUS: &str = "org.mpris.MediaPlayer2.com.blitzfc.qbz";

#[zbus::proxy(
	interface = "org.mpris.MediaPlayer2.Player",
	default_service = "org.mpris.MediaPlayer2.com.blitzfc.qbz",
	default_path = "/org/mpris/MediaPlayer2"
)]
pub trait Player {
	fn play_pause(&self) -> zbus::Result<()>;
	fn play(&self) -> zbus::Result<()>;
	fn pause(&self) -> zbus::Result<()>;
	fn stop(&self) -> zbus::Result<()>;
	fn next(&self) -> zbus::Result<()>;
	fn previous(&self) -> zbus::Result<()>;
	/// Relative seek, in microseconds.
	fn seek(&self, offset: i64) -> zbus::Result<()>;

	#[zbus(property)]
	fn playback_status(&self) -> zbus::Result<String>;

	#[zbus(property)]
	fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

	/// Microseconds. Upstream extrapolates this from a timestamp while
	/// playing, so it is accurate between `PropertiesChanged` bursts.
	///
	/// Declared as never emitting a change signal (which matches the MPRIS
	/// spec and QBZ's own introspection) so zbus refetches instead of
	/// serving a cached value that would freeze the progress display.
	#[zbus(property(emits_changed_signal = "false"))]
	fn position(&self) -> zbus::Result<i64>;

	/// Reading this is useless (always `1.0`); writing it works.
	#[zbus(property)]
	fn set_volume(&self, volume: f64) -> zbus::Result<()>;
}

/// The subset of `xesam:`/`mpris:` metadata this plugin renders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackInfo {
	pub title: String,
	pub artist: String,
	pub album: String,
	/// `mpris:trackid`, a monotonic path upstream. Used as a cache key.
	pub track_id: String,
	/// Qobuz catalogue id, parsed out of `xesam:url`. `None` for local,
	/// Plex and Jellyfin sources, which have no Qobuz identity to favourite.
	pub qobuz_id: Option<u64>,
	pub art_url: Option<String>,
	/// Track length in microseconds; `0` when upstream omits it.
	pub length_us: i64,
}

impl TrackInfo {
	/// Build from a raw MPRIS metadata dictionary, tolerating every field
	/// being absent — upstream omits empty artists and zero lengths.
	pub fn from_metadata(md: &HashMap<String, OwnedValue>) -> Self {
		let string = |key: &str| -> String {
			md.get(key)
				.and_then(|v| <&str>::try_from(v).ok())
				.unwrap_or_default()
				.to_string()
		};

		// `xesam:artist` is an array of strings; QBZ always sends exactly one.
		let artist = md
			.get("xesam:artist")
			.and_then(|v| Vec::<String>::try_from(v.clone()).ok())
			.and_then(|v| v.into_iter().next())
			.unwrap_or_default();

		let url = string("xesam:url");
		let qobuz_id = url
			.rsplit_once("/track/")
			.and_then(|(_, id)| id.parse::<u64>().ok());

		let art_url = match string("mpris:artUrl") {
			s if s.is_empty() => None,
			s => Some(s),
		};

		// `mpris:trackid` is an object path, not a string.
		let track_id = md
			.get("mpris:trackid")
			.and_then(|v| zbus::zvariant::ObjectPath::try_from(v.clone()).ok())
			.map(|p| p.as_str().to_string())
			.unwrap_or_default();

		Self {
			title: string("xesam:title"),
			artist,
			album: string("xesam:album"),
			track_id,
			qobuz_id,
			art_url,
			length_us: md
				.get("mpris:length")
				.and_then(|v| i64::try_from(v).ok())
				.unwrap_or(0),
		}
	}
}

/// Playback state as reported by `PlaybackStatus`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Playback {
	Playing,
	Paused,
	#[default]
	Stopped,
}

impl Playback {
	pub fn parse(s: &str) -> Self {
		match s {
			"Playing" => Self::Playing,
			"Paused" => Self::Paused,
			_ => Self::Stopped,
		}
	}

	pub fn is_playing(self) -> bool {
		self == Self::Playing
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use zbus::zvariant::{OwnedValue, Value};

	use super::{Playback, TrackInfo};

	fn value(v: impl Into<Value<'static>>) -> OwnedValue {
		OwnedValue::try_from(v.into()).unwrap()
	}

	fn qbz_metadata() -> HashMap<String, OwnedValue> {
		// Shaped exactly like a live reading from QBZ.
		HashMap::from([
			("xesam:title".into(), value("California Blue")),
			("xesam:album".into(), value("California Blue")),
			("xesam:artist".into(), value(vec!["Elias Dris".to_string()])),
			(
				"xesam:url".into(),
				value("https://open.qobuz.com/track/446424286"),
			),
			(
				"mpris:artUrl".into(),
				value("https://static.qobuz.com/images/covers/68/7f/d447ybl9x7f68_600.jpg"),
			),
			("mpris:length".into(), value(216_000_000i64)),
			(
				"mpris:trackid".into(),
				value(zbus::zvariant::ObjectPath::try_from("/com/blitzfc/qbz/track/69").unwrap()),
			),
		])
	}

	#[test]
	fn reads_every_field_from_a_live_metadata_dictionary() {
		let track = TrackInfo::from_metadata(&qbz_metadata());
		assert_eq!(track.title, "California Blue");
		assert_eq!(track.album, "California Blue");
		assert_eq!(track.artist, "Elias Dris");
		assert_eq!(track.length_us, 216_000_000);
		assert_eq!(track.track_id, "/com/blitzfc/qbz/track/69");
	}

	/// The catalogue id is the key to favouriting, and the only place it
	/// appears is inside `xesam:url`.
	#[test]
	fn pulls_the_qobuz_id_out_of_the_track_url() {
		let track = TrackInfo::from_metadata(&qbz_metadata());
		assert_eq!(track.qobuz_id, Some(446_424_286));
	}

	/// Local, Plex and Jellyfin tracks have no Qobuz identity, so the
	/// favourite key must be able to tell that they cannot be favourited.
	#[test]
	fn a_track_with_no_qobuz_url_has_no_id() {
		let mut md = qbz_metadata();
		md.remove("xesam:url");
		assert_eq!(TrackInfo::from_metadata(&md).qobuz_id, None);

		md.insert("xesam:url".into(), value("file:///music/song.flac"));
		assert_eq!(TrackInfo::from_metadata(&md).qobuz_id, None);
	}

	/// Upstream omits empty artists and zero lengths rather than sending
	/// blanks, so every field has to be optional.
	#[test]
	fn an_empty_dictionary_yields_an_empty_track() {
		let track = TrackInfo::from_metadata(&HashMap::new());
		assert_eq!(track, TrackInfo::default());
	}

	#[test]
	fn missing_artwork_is_none_rather_than_an_empty_string() {
		let mut md = qbz_metadata();
		md.insert("mpris:artUrl".into(), value(""));
		assert_eq!(TrackInfo::from_metadata(&md).art_url, None);
	}

	#[test]
	fn unknown_playback_states_are_treated_as_stopped() {
		assert_eq!(Playback::parse("Playing"), Playback::Playing);
		assert_eq!(Playback::parse("Paused"), Playback::Paused);
		assert_eq!(Playback::parse("Stopped"), Playback::Stopped);
		assert_eq!(Playback::parse("nonsense"), Playback::Stopped);
	}
}
