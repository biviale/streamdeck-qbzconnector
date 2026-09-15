//! The in-app volume of QBZ, tracked rather than read.
//!
//! QBZ's MPRIS `Volume` property is write-only in practice: the setter
//! forwards to the audio engine, but the getter answers the `1.0` it was
//! initialised with forever, because `qbz-qt` never pushes the level back
//! into its MPRIS state. Live reading: `busctl` said `1.0` while the real
//! level was `0.009`.
//!
//! So the plugin keeps its own value and re-anchors it wherever it can:
//!
//! 1. **Seed** from `~/.local/share/qbz/ui_prefs.json`, the level QBZ
//!    restores at startup.
//! 2. **Adopt** changes to that file, which QBZ writes when the user
//!    releases its volume slider.
//! 3. **Own** every change the plugin makes: an absolute write is exact.
//!
//! QBZ's in-app keyboard shortcuts persist nothing, so the value can still
//! drift; a long press on a volume key re-reads the file to recover.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hub::hub;

/// Level QBZ falls back to when nothing has been persisted yet.
const DEFAULT_LEVEL: f64 = 0.5;

/// Levels closer together than this are the same level. Covers both the
/// f32 to f64 widening QBZ's value goes through and the 0.1 % granularity a
/// key can actually display.
const SAME_LEVEL: f64 = 0.0005;

/// Lowest non-zero level the perceptual curve will step to.
///
/// A multiplicative curve can never climb away from exactly zero, so an
/// increase from silence jumps here first. Low enough to be inaudible, high
/// enough that a few more steps reach a usable level.
const PERCEPTUAL_FLOOR: f64 = 0.0005;

/// `~/.local/share/qbz/ui_prefs.json`
fn prefs_path() -> Option<PathBuf> {
	Some(dirs::data_dir()?.join("qbz").join("ui_prefs.json"))
}

/// Read the persisted level, or `None` if QBZ has never written one.
pub fn read_persisted() -> Option<f64> {
	let text = std::fs::read_to_string(prefs_path()?).ok()?;
	let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
	doc.get("volume")?.as_f64().map(|v| v.clamp(0.0, 1.0))
}

/// How a step changes the level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Curve {
	/// Add or subtract a fixed number of percentage points. Matches what QBZ
	/// itself does everywhere (its own steppers move by a flat 0.05).
	#[default]
	Linear,
	/// Multiply by a fixed number of decibels.
	///
	/// QBZ's gain is a linear amplitude multiplier (rodio's `set_volume`),
	/// but hearing is logarithmic, so a flat 5-point step is a rounding error
	/// at the top of the range and a several-fold jump at the bottom. This
	/// curve keeps every step feeling the same size.
	Perceptual,
}

/// Per-instance step configuration, shared by the volume up/down keys and the
/// encoder.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StepSettings {
	pub curve: Curve,
	/// Step size for [`Curve::Linear`], in percentage points.
	pub step_percent: f64,
	/// Step size for [`Curve::Perceptual`], in decibels.
	pub step_db: f64,
	/// Whether to write the current level as the key's title.
	pub show_percent: bool,
}

impl Default for StepSettings {
	fn default() -> Self {
		Self {
			curve: Curve::Linear,
			step_percent: 5.0,
			step_db: 2.0,
			show_percent: true,
		}
	}
}

impl StepSettings {
	/// Apply one step in `direction` (+1 up, -1 down) to `level`.
	pub fn apply(&self, level: f64, direction: f64) -> f64 {
		let next = match self.curve {
			Curve::Linear => {
				let step = (self.step_percent / 100.0).abs().max(0.001);
				level + step * direction
			}
			Curve::Perceptual => {
				let db = self.step_db.abs().max(0.1) * direction;
				if level <= 0.0 {
					// Nothing to multiply; step off the floor instead.
					if direction > 0.0 {
						PERCEPTUAL_FLOOR
					} else {
						0.0
					}
				} else {
					let scaled = level * 10f64.powf(db / 20.0);
					// Below the floor on the way down is indistinguishable
					// from silence, so snap rather than crawl.
					if scaled < PERCEPTUAL_FLOOR && direction < 0.0 {
						0.0
					} else {
						scaled
					}
				}
			}
		};
		next.clamp(0.0, 1.0)
	}
}

/// Seed the shadow from disk and start adopting later changes to that file.
pub async fn start() {
	let seed = read_persisted().unwrap_or(DEFAULT_LEVEL);
	*LAST_PERSISTED.lock().unwrap() = read_persisted();
	hub().update(|s| s.volume = seed);
	log::info!("volume seeded from ui_prefs.json at {:.1}%", seed * 100.0);

	if let Err(error) = spawn_prefs_watcher() {
		// Not fatal: the plugin still owns every change it makes itself,
		// it just cannot notice changes made in QBZ's own UI.
		log::warn!("cannot watch ui_prefs.json, volume may drift: {error}");
	}
}

/// Watch for QBZ persisting a new level.
///
/// The watch is on the **directory**, not the file: QBZ replaces
/// `ui_prefs.json` by atomic rename, so a watch bound to the original inode
/// would stop firing after the very first save.
fn spawn_prefs_watcher() -> notify::Result<()> {
	use notify::{RecursiveMode, Watcher};

	let Some(path) = prefs_path() else {
		return Ok(());
	};
	let Some(dir) = path.parent().map(PathBuf::from) else {
		return Ok(());
	};
	let file_name = path.file_name().map(|n| n.to_os_string());

	let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(8);
	let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
		let Ok(event) = event else { return };
		if is_prefs_write(&event, file_name.as_deref()) {
			// Coalescing is fine: the handler re-reads the file, so a burst
			// of rename events collapses into one read.
			let _ = tx.try_send(());
		}
	})?;
	watcher.watch(&dir, RecursiveMode::NonRecursive)?;

	tokio::spawn(async move {
		// Keep the watcher alive for as long as the task runs; dropping it
		// would silently stop delivery.
		let _watcher = watcher;
		while rx.recv().await.is_some() {
			adopt_persisted().await;
		}
	});

	Ok(())
}

/// The level last read from disk, so a rewrite that leaves the volume alone
/// can be told apart from one that changes it.
static LAST_PERSISTED: std::sync::Mutex<Option<f64>> = std::sync::Mutex::new(None);

/// Whether a level just read from disk is news.
///
/// Every QBZ preference goes through the same whole-file writer, so changing
/// the theme rewrites `ui_prefs.json` with whatever volume it last recorded —
/// a value usually older than the plugin's, since QBZ does not persist the
/// changes made from here. Adopting it every time would rewind the user.
fn is_new_persisted(previous: Option<f64>, level: f64) -> bool {
	match previous {
		Some(previous) => (previous - level).abs() >= SAME_LEVEL,
		// Nothing read yet, so there is nothing to contradict.
		None => true,
	}
}

/// Whether an event means the preferences file was actually rewritten.
///
/// The kind matters as much as the path: reading a file emits `Access`
/// events, and this module reads `ui_prefs.json` every time it wakes, so
/// reacting to any event makes the watcher feed itself and drag the level
/// back after every key press.
fn is_prefs_write(event: &notify::Event, file_name: Option<&std::ffi::OsStr>) -> bool {
	if !event.paths.iter().any(|p| p.file_name() == file_name) {
		return false;
	}
	// QBZ replaces the file by atomic rename, which arrives as a create or a
	// rename-to; a plain write arrives as a modify. Nothing else is a change.
	event.kind.is_create() || event.kind.is_modify()
}

/// Take the persisted level as truth, if QBZ has recorded a new one.
///
/// This is the passive path, driven by the file watcher.
pub async fn adopt_persisted() {
	adopt(false).await
}

/// Re-read the persisted level and take it, whether or not it looks new.
///
/// The deliberate path, behind a long press: the user is saying the display
/// has drifted, so the "is this new" test that protects the passive path
/// would only get in the way.
pub async fn resync_from_disk() {
	adopt(true).await
}

async fn adopt(force: bool) {
	let Some(level) = read_persisted() else { return };

	{
		let mut last = LAST_PERSISTED.lock().unwrap();
		if !force && !is_new_persisted(*last, level) {
			return;
		}
		*last = Some(level);
	}

	let _guard = hub().volume_lock.lock().await;
	let current = hub().state().volume;
	if (current - level).abs() < SAME_LEVEL {
		return;
	}

	log::info!(
		"adopting volume {:.1}% from QBZ (was {:.1}%)",
		level * 100.0,
		current * 100.0
	);
	hub().update(|s| {
		s.volume = level;
		// An explicit non-zero level from QBZ means it is not muted.
		if level > 0.0 {
			s.muted = false;
		}
	});
}

/// Write an absolute level to QBZ and take it as the new truth.
pub async fn set(level: f64) -> zbus::Result<f64> {
	let level = level.clamp(0.0, 1.0);
	let _guard = hub().volume_lock.lock().await;
	write(level).await?;
	hub().update(|s| {
		s.volume = level;
		// An explicit level is never a mute, even when it is zero: the mute
		// key is the only thing that sets that flag.
		s.muted = false;
	});
	Ok(level)
}

/// Move the level by one step in `direction` (+1 up, -1 down).
pub async fn step(settings: &StepSettings, direction: f64) -> zbus::Result<f64> {
	let _guard = hub().volume_lock.lock().await;
	let state = hub().state();
	// `volume` holds the pre-mute level while muted, so stepping resumes
	// from where the user was rather than from the zero on the wire.
	let next = settings.apply(state.volume, direction);

	write(next).await?;
	hub().update(|s| {
		s.volume = next;
		s.muted = false;
	});
	Ok(next)
}

/// Toggle the simulated mute.
///
/// MPRIS exposes no mute, and QBZ's own mute is not on the bus, so this
/// writes silence and remembers what to come back to.
pub async fn toggle_mute() -> zbus::Result<bool> {
	let _guard = hub().volume_lock.lock().await;
	let state = hub().state();

	if state.muted {
		// Restore exactly what was there before the mute. Substituting a
		// default when that was zero would be a nasty surprise for someone
		// listening quietly.
		let restore = state.volume;
		write(restore).await?;
		hub().update(|s| s.muted = false);
		Ok(false)
	} else {
		write(0.0).await?;
		// `volume` keeps the pre-mute level: it is what unmute restores and
		// what the key displays.
		hub().update(|s| s.muted = true);
		Ok(true)
	}
}

/// Push a level onto the bus.
async fn write(level: f64) -> zbus::Result<()> {
	hub().player().await?.set_volume(level).await
}

#[cfg(test)]
mod tests {
	use super::*;

	fn linear(step_percent: f64) -> StepSettings {
		StepSettings {
			curve: Curve::Linear,
			step_percent,
			..StepSettings::default()
		}
	}

	fn perceptual(step_db: f64) -> StepSettings {
		StepSettings {
			curve: Curve::Perceptual,
			step_db,
			..StepSettings::default()
		}
	}

	#[test]
	fn a_linear_step_adds_percentage_points() {
		let settings = linear(5.0);
		assert!((settings.apply(0.50, 1.0) - 0.55).abs() < 1e-9);
		assert!((settings.apply(0.50, -1.0) - 0.45).abs() < 1e-9);
	}

	#[test]
	fn levels_never_leave_the_zero_to_one_range() {
		let settings = linear(20.0);
		assert_eq!(settings.apply(0.95, 1.0), 1.0);
		assert_eq!(settings.apply(0.05, -1.0), 0.0);
	}

	/// The reason the perceptual curve exists: at a very low level a flat
	/// 5-point step multiplies the volume several times over, while a
	/// decibel step stays proportional.
	#[test]
	fn a_perceptual_step_is_proportional_at_low_levels() {
		let quiet = 0.009;
		let jump = linear(5.0).apply(quiet, 1.0) / quiet;
		assert!(
			jump > 6.0,
			"linear step at a low level should be a huge jump, was {jump}x"
		);

		let gentle = perceptual(2.0).apply(quiet, 1.0) / quiet;
		assert!(
			(gentle - 1.2589).abs() < 0.001,
			"+2 dB should be about 1.26x, was {gentle}x"
		);
	}

	#[test]
	fn a_perceptual_step_is_the_same_ratio_at_every_level() {
		let settings = perceptual(3.0);
		let low = settings.apply(0.01, 1.0) / 0.01;
		let high = settings.apply(0.40, 1.0) / 0.40;
		assert!((low - high).abs() < 1e-9);
	}

	#[test]
	fn a_perceptual_step_can_climb_away_from_silence() {
		// Multiplying zero would leave it at zero forever, so the first step
		// up lands on the floor instead.
		assert_eq!(perceptual(2.0).apply(0.0, 1.0), PERCEPTUAL_FLOOR);
		assert_eq!(perceptual(2.0).apply(0.0, -1.0), 0.0);
	}

	#[test]
	fn a_perceptual_step_down_snaps_to_silence_below_the_floor() {
		let settings = perceptual(6.0);
		assert_eq!(settings.apply(PERCEPTUAL_FLOOR * 1.1, -1.0), 0.0);
	}

	#[test]
	fn a_zero_step_is_rejected_so_a_key_always_does_something() {
		// A misconfigured step of 0 would make the key look broken.
		assert!(linear(0.0).apply(0.5, 1.0) > 0.5);
		assert!(perceptual(0.0).apply(0.5, 1.0) > 0.5);
	}
}

#[cfg(test)]
mod watcher_tests {
	use std::ffi::OsString;
	use std::path::PathBuf;

	use notify::event::{CreateKind, EventKind, ModifyKind, RenameMode};

	use super::is_prefs_write;

	fn event(kind: EventKind, name: &str) -> notify::Event {
		notify::Event::new(kind).add_path(PathBuf::from("/home/u/.local/share/qbz").join(name))
	}

	fn prefs() -> OsString {
		OsString::from("ui_prefs.json")
	}

	/// `adopt_persisted` opens the file, which emits an access event: react
	/// to that and the watcher feeds itself.
	#[test]
	fn reading_the_file_is_not_a_change() {
		let name = prefs();
		assert!(!is_prefs_write(
			&event(
				EventKind::Access(notify::event::AccessKind::Open(notify::event::AccessMode::Any)),
				"ui_prefs.json"
			),
			Some(&name)
		));
	}

	#[test]
	fn an_atomic_rename_into_place_is_a_change() {
		let name = prefs();
		assert!(is_prefs_write(
			&event(EventKind::Create(CreateKind::File), "ui_prefs.json"),
			Some(&name)
		));
		assert!(is_prefs_write(
			&event(
				EventKind::Modify(ModifyKind::Name(RenameMode::To)),
				"ui_prefs.json"
			),
			Some(&name)
		));
	}

	/// The watch is on the whole directory, which also holds QBZ's very busy
	/// SQLite write-ahead logs.
	#[test]
	fn changes_to_other_files_in_the_directory_are_ignored() {
		let name = prefs();
		assert!(!is_prefs_write(
			&event(EventKind::Create(CreateKind::File), "library.db-wal"),
			Some(&name)
		));
	}
}

#[cfg(test)]
mod live_tests {
	//! Checks against a running QBZ. Ignored by default because they need a
	//! live session and move the real volume:
	//!
	//! ```text
	//! cargo test --release -- --ignored steps_accumulate --nocapture
	//! ```
	use super::*;

	/// Twelve steps up must land twelve steps higher, and stay there: a
	/// watcher that reacts to its own reads drags every step back.
	#[tokio::test]
	#[ignore]
	async fn steps_accumulate_instead_of_snapping_back() {
		crate::hub::start().await.unwrap();
		tokio::time::sleep(std::time::Duration::from_millis(500)).await;
		start().await;

		let original = hub().state().volume;
		// Small enough to stay inaudible on a live session.
		let settings = StepSettings {
			curve: Curve::Linear,
			step_percent: 0.2,
			..StepSettings::default()
		};
		const STEPS: usize = 12;

		for _ in 0..STEPS {
			step(&settings, 1.0).await.unwrap();
		}
		let climbed = hub().state().volume;

		// Let any stray watcher activity land before checking.
		tokio::time::sleep(std::time::Duration::from_millis(800)).await;
		let settled = hub().state().volume;

		set(original).await.unwrap();

		let expected = (original + 0.002 * STEPS as f64).min(1.0);
		assert!(
			(climbed - expected).abs() < 1e-6,
			"after {STEPS} steps expected {expected:.4}, got {climbed:.4}"
		);
		assert!(
			(settled - climbed).abs() < 1e-6,
			"the level was pulled back to {settled:.4} after settling at {climbed:.4}"
		);
	}
}

#[cfg(test)]
mod adoption_tests {
	use super::{is_new_persisted, SAME_LEVEL};

	#[test]
	fn the_first_reading_is_always_news() {
		assert!(is_new_persisted(None, 0.42));
	}

	#[test]
	fn a_genuinely_new_level_is_adopted() {
		assert!(is_new_persisted(Some(0.30), 0.55));
	}

	/// Changing any other QBZ setting rewrites the whole file, carrying an
	/// old volume with it. Adopting that would rewind the user.
	#[test]
	fn an_unrelated_rewrite_carrying_the_same_level_is_not_adopted() {
		assert!(!is_new_persisted(Some(0.30), 0.30));
	}

	#[test]
	fn differences_too_small_to_display_are_not_adopted() {
		assert!(!is_new_persisted(Some(0.30), 0.30 + SAME_LEVEL / 2.0));
		assert!(is_new_persisted(Some(0.30), 0.30 + SAME_LEVEL * 2.0));
	}
}
