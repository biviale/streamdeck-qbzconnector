//! Painting a key from a glyph and a colour.
//!
//! With the album tint on, the plugin supplies the key image itself instead
//! of letting the server use the tile from the manifest. The shapes are the
//! ones in `icons/`: `scripts/make_icons.py` writes both those tiles and the
//! white-on-transparent masks embedded here, so the two cannot drift.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine as _;
use image::RgbaImage;

use crate::render::KEY_SIZE;

/// The shapes a key can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Glyph {
	Play,
	Pause,
	Stop,
	Next,
	Previous,
	NowPlaying,
	Seek,
	SeekBackwards,
	Volume,
	VolumeUp,
	VolumeDown,
	VolumeSet,
	Muted,
	FavoriteOff,
}

impl Glyph {
	fn mask(self) -> &'static [u8] {
		match self {
			Self::Play => include_bytes!("../assets/glyphs/play.png"),
			Self::Pause => include_bytes!("../assets/glyphs/pause.png"),
			Self::Stop => include_bytes!("../assets/glyphs/stop.png"),
			Self::Next => include_bytes!("../assets/glyphs/next.png"),
			Self::Previous => include_bytes!("../assets/glyphs/previous.png"),
			Self::NowPlaying => include_bytes!("../assets/glyphs/nowplaying.png"),
			Self::Seek => include_bytes!("../assets/glyphs/seek.png"),
			Self::SeekBackwards => include_bytes!("../assets/glyphs/seek_backwards.png"),
			Self::Volume => include_bytes!("../assets/glyphs/volume.png"),
			Self::VolumeUp => include_bytes!("../assets/glyphs/volume_up.png"),
			Self::VolumeDown => include_bytes!("../assets/glyphs/volume_down.png"),
			Self::VolumeSet => include_bytes!("../assets/glyphs/volume_set.png"),
			Self::Muted => include_bytes!("../assets/glyphs/muted.png"),
			Self::FavoriteOff => include_bytes!("../assets/glyphs/favorite_off.png"),
		}
	}
}

/// Decoded masks, so a mask is only turned into pixels once per run.
static MASKS: Mutex<Option<HashMap<Glyph, RgbaImage>>> = Mutex::new(None);

/// A painted key, identified by its shape and its background.
type KeyId = (Glyph, [u8; 3]);

/// Finished keys, by glyph and colour. The colour only changes when the
/// album does, so this settles immediately and stays small.
static KEYS: Mutex<Option<HashMap<KeyId, String>>> = Mutex::new(None);

/// Above this the cache is cleared rather than grown: it exists to make one
/// album's redraws free, not to remember every album of the session.
const CACHE_LIMIT: usize = 64;

/// A key showing `glyph` in white on `colour`, as a PNG data URL.
///
/// Returns `None` only if an embedded mask fails to decode, which would mean
/// the binary itself is damaged; callers fall back to the manifest tile.
pub fn tinted(glyph: Glyph, colour: [u8; 3]) -> Option<String> {
	if let Some(hit) = KEYS
		.lock()
		.unwrap()
		.as_ref()
		.and_then(|map| map.get(&(glyph, colour)))
	{
		return Some(hit.clone());
	}

	let rendered = paint(glyph, colour)?;

	let mut guard = KEYS.lock().unwrap();
	let map = guard.get_or_insert_with(HashMap::new);
	if map.len() >= CACHE_LIMIT {
		map.clear();
	}
	map.insert((glyph, colour), rendered.clone());
	Some(rendered)
}

fn paint(glyph: Glyph, colour: [u8; 3]) -> Option<String> {
	let mask = decoded(glyph)?;
	let mut key = RgbaImage::from_pixel(
		KEY_SIZE,
		KEY_SIZE,
		image::Rgba([colour[0], colour[1], colour[2], 255]),
	);

	for (x, y, pixel) in mask.enumerate_pixels() {
		if x >= KEY_SIZE || y >= KEY_SIZE {
			continue;
		}
		// The mask is white with a soft alpha edge; that alpha is exactly the
		// coverage to blend the glyph in with, so anti-aliasing carries over
		// from the SVG for free.
		let coverage = pixel[3] as f32 / 255.0;
		if coverage <= 0.0 {
			continue;
		}
		let out = key.get_pixel_mut(x, y);
		for channel in 0..3 {
			let base = out[channel] as f32;
			let glyph_channel = pixel[channel] as f32;
			out[channel] = (base + (glyph_channel - base) * coverage).round() as u8;
		}
	}

	let mut png = Vec::new();
	key.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
		.ok()?;
	Some(format!(
		"data:image/png;base64,{}",
		base64::engine::general_purpose::STANDARD.encode(&png)
	))
}

fn decoded(glyph: Glyph) -> Option<RgbaImage> {
	let mut guard = MASKS.lock().unwrap();
	let map = guard.get_or_insert_with(HashMap::new);
	if let Some(mask) = map.get(&glyph) {
		return Some(mask.clone());
	}
	let mask = image::load_from_memory(glyph.mask()).ok()?.to_rgba8();
	map.insert(glyph, mask.clone());
	Some(mask)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every mask must decode at the size the key expects — a mismatch would
	/// silently crop or letterbox a glyph.
	#[test]
	fn every_embedded_mask_decodes_at_key_size() {
		for glyph in [
			Glyph::Play,
			Glyph::Pause,
			Glyph::Stop,
			Glyph::Next,
			Glyph::Previous,
			Glyph::NowPlaying,
			Glyph::Seek,
			Glyph::SeekBackwards,
			Glyph::Volume,
			Glyph::VolumeUp,
			Glyph::VolumeDown,
			Glyph::VolumeSet,
			Glyph::Muted,
			Glyph::FavoriteOff,
		] {
			let mask = decoded(glyph).unwrap_or_else(|| panic!("{glyph:?} failed to decode"));
			assert_eq!(
				(mask.width(), mask.height()),
				(KEY_SIZE, KEY_SIZE),
				"{glyph:?} is {}x{}",
				mask.width(),
				mask.height()
			);
		}
	}

	#[test]
	fn a_painted_key_uses_the_requested_colour_in_its_corners() {
		let data = tinted(Glyph::Play, [10, 60, 90]).unwrap();
		let png = base64::engine::general_purpose::STANDARD
			.decode(data.split_once(',').unwrap().1)
			.unwrap();
		let image = image::load_from_memory(&png).unwrap().to_rgba8();

		// A corner is always background: no glyph reaches it.
		let corner = image.get_pixel(2, 2);
		assert_eq!([corner[0], corner[1], corner[2]], [10, 60, 90]);
	}

	/// The glyph has to actually be drawn, not just the background filled.
	#[test]
	fn a_painted_key_contains_white_pixels_where_the_glyph_is() {
		let data = tinted(Glyph::Play, [10, 60, 90]).unwrap();
		let png = base64::engine::general_purpose::STANDARD
			.decode(data.split_once(',').unwrap().1)
			.unwrap();
		let image = image::load_from_memory(&png).unwrap().to_rgba8();

		let white = image
			.pixels()
			.filter(|p| p[0] > 240 && p[1] > 240 && p[2] > 240)
			.count();
		assert!(
			white > 500,
			"expected a visible glyph, found {white} white pixels"
		);
	}
}
