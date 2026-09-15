//! Turning cover art into something a key can display.
//!
//! QBZ publishes `mpris:artUrl` either as an HTTPS URL on Qobuz's CDN
//! (rewritten upstream to the 600 px tier) or as a `file://` path for local,
//! Plex and Jellyfin sources. Both are handled here.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use image::imageops::FilterType;

/// Keys are rendered at 2x so they stay sharp on hardware that asks for it.
pub const KEY_SIZE: u32 = 144;

/// How far up the key the darkening ramp reaches, as a fraction of height.
/// Enough to seat a two-line title without hiding the artwork.
const SCRIM_HEIGHT: f32 = 0.45;

/// Strongest darkening, at the very bottom edge.
const SCRIM_MAX: f32 = 0.72;

/// Rendered keys, by source URL.
///
/// Covers are fetched and re-encoded, too much work to repeat on every
/// metadata change. Bounded so a long session cannot grow it without limit.
static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Most recently rendered covers to keep.
const CACHE_LIMIT: usize = 24;

/// Raw bytes of recently fetched covers.
///
/// One cover feeds two consumers — the key image and the album colour — and
/// they are asked for at different moments, so without this the same image
/// would be downloaded twice per track.
static BYTES: Mutex<Option<HashMap<String, std::sync::Arc<Vec<u8>>>>> = Mutex::new(None);

/// Covers whose bytes are kept. Small: this only has to bridge the gap
/// between the two consumers of the current track's art.
const BYTES_LIMIT: usize = 4;

fn cached(url: &str) -> Option<String> {
	CACHE.lock().unwrap().as_ref()?.get(url).cloned()
}

fn remember(url: &str, data_url: String) {
	let mut guard = CACHE.lock().unwrap();
	let map = guard.get_or_insert_with(HashMap::new);
	if map.len() >= CACHE_LIMIT {
		// No recency tracking: the cache exists to survive a burst of
		// metadata events for one track, not to be an archive.
		map.clear();
	}
	map.insert(url.to_string(), data_url);
}

/// A transport glyph stamped onto the cover.
///
/// A key that shows artwork has no room left for the state image from the
/// manifest, so the state has to be drawn onto the art itself — otherwise a
/// play/pause key showing a cover would say nothing about what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
	Play,
	Pause,
}

impl Badge {
	fn tag(self) -> &'static str {
		match self {
			Self::Play => "play",
			Self::Pause => "pause",
		}
	}
}

/// Render cover art as a base64 PNG data URL, ready for `setImage`.
///
/// Returns `None` if the art cannot be fetched or decoded, in which case the
/// caller should fall back to the manifest's own image.
pub async fn cover(url: &str, scrim: bool, badge: Option<Badge>) -> Option<String> {
	if url.is_empty() {
		return None;
	}
	let cache_key = format!("{url}|{scrim}|{}", badge.map(Badge::tag).unwrap_or("none"));
	if let Some(hit) = cached(&cache_key) {
		return Some(hit);
	}

	let bytes = fetch_cached(url).await?;
	// Decoding and resizing are CPU-bound and can take tens of milliseconds
	// on a large cover, which would stall the event loop.
	let rendered = tokio::task::spawn_blocking(move || encode(&bytes, scrim, badge))
		.await
		.ok()
		.flatten()?;

	remember(&cache_key, rendered.clone());
	Some(rendered)
}

/// Fetch the art, reusing the bytes if they were just downloaded.
async fn fetch_cached(url: &str) -> Option<std::sync::Arc<Vec<u8>>> {
	if let Some(hit) = BYTES.lock().unwrap().as_ref().and_then(|m| m.get(url)) {
		return Some(hit.clone());
	}

	let bytes = std::sync::Arc::new(fetch(url).await?);

	let mut guard = BYTES.lock().unwrap();
	let map = guard.get_or_insert_with(HashMap::new);
	if map.len() >= BYTES_LIMIT {
		map.clear();
	}
	map.insert(url.to_string(), bytes.clone());
	Some(bytes)
}

/// The album colour for a cover, ready to tint keys with.
///
/// Returns `None` when the art cannot be fetched, which callers treat as
/// "leave the keys alone" rather than substituting a colour.
pub async fn album_colour(url: &str) -> Option<[u8; 3]> {
	if url.is_empty() {
		return None;
	}
	let bytes = fetch_cached(url).await?;
	// Decoding plus the pixel scan is CPU-bound.
	tokio::task::spawn_blocking(move || {
		let image = image::load_from_memory(&bytes).ok()?;
		Some(crate::palette::accent(&image))
	})
	.await
	.ok()
	.flatten()
}

/// Read the art, from disk or over the network.
async fn fetch(url: &str) -> Option<Vec<u8>> {
	if let Some(path) = file_url_to_path(url) {
		return tokio::fs::read(path).await.ok();
	}
	if !url.starts_with("http://") && !url.starts_with("https://") {
		return None;
	}

	// A cover is decoration: it must never hold up a key redraw for long.
	let client = reqwest::Client::builder()
		.timeout(Duration::from_secs(8))
		.build()
		.ok()?;
	let response = client.get(url).send().await.ok()?;
	if !response.status().is_success() {
		log::debug!("cover fetch returned {} for {url}", response.status());
		return None;
	}
	response.bytes().await.ok().map(|b| b.to_vec())
}

/// Convert a `file://` URL to a path, undoing percent-encoding.
///
/// Upstream builds these with `ArtworkRef::to_mpris_url`, which percent-
/// encodes the path, so a cover in a directory with a space or an accent
/// would otherwise fail to open.
fn file_url_to_path(url: &str) -> Option<std::path::PathBuf> {
	let encoded = url.strip_prefix("file://")?;
	let bytes = encoded.as_bytes();
	let mut out = Vec::with_capacity(bytes.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] == b'%' && i + 2 < bytes.len() {
			let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
			// A stray '%' that is not a valid escape is a literal one.
			if let Ok(byte) = u8::from_str_radix(hex, 16) {
				out.push(byte);
				i += 3;
				continue;
			}
		}
		out.push(bytes[i]);
		i += 1;
	}
	String::from_utf8(out).ok().map(std::path::PathBuf::from)
}

/// Crop to a square, resize to key size, optionally darken the bottom and
/// stamp a transport badge.
fn encode(bytes: &[u8], scrim: bool, badge: Option<Badge>) -> Option<String> {
	let image = image::load_from_memory(bytes).ok()?;
	// `resize_to_fill` crops rather than distorting, which matters: a cover
	// that is not perfectly square is common for local files.
	let mut rgba = image
		.resize_to_fill(KEY_SIZE, KEY_SIZE, FilterType::Lanczos3)
		.to_rgba8();

	if scrim {
		apply_scrim(&mut rgba);
	}
	if let Some(badge) = badge {
		draw_badge(&mut rgba, badge);
	}

	let mut png = Vec::new();
	rgba.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
		.ok()?;

	Some(format!(
		"data:image/png;base64,{}",
		base64::engine::general_purpose::STANDARD.encode(&png)
	))
}

/// Darken the bottom of the key so a title stays readable over bright art.
fn apply_scrim(image: &mut image::RgbaImage) {
	let height = image.height() as f32;
	let start = height * (1.0 - SCRIM_HEIGHT);

	for y in 0..image.height() {
		let depth = (y as f32 - start) / (height - start);
		if depth <= 0.0 {
			continue;
		}
		// Squared so the ramp stays subtle for most of its run and only
		// deepens near the edge, instead of greying out half the cover.
		let factor = 1.0 - SCRIM_MAX * depth * depth;
		for x in 0..image.width() {
			let px = image.get_pixel_mut(x, y);
			px[0] = (px[0] as f32 * factor) as u8;
			px[1] = (px[1] as f32 * factor) as u8;
			px[2] = (px[2] as f32 * factor) as u8;
		}
	}
}

// --- Transport badge -------------------------------------------------------
//
// Drawn by hand: a triangle and two bars in a circle are not worth an SVG
// renderer. Coverage is supersampled so the edges do not look ragged.

/// Badge disc radius, as a fraction of the key's width.
const BADGE_RADIUS: f32 = 0.19;

/// Gap between the disc and the key edge, as a fraction of the key's width.
const BADGE_MARGIN: f32 = 0.055;

/// Opacity of the disc behind the glyph. High enough that a white glyph
/// survives a bright cover, low enough to let the art show through.
const BADGE_OPACITY: f32 = 0.66;

/// Samples per axis when estimating how much of a pixel a shape covers.
const SUPERSAMPLE: u32 = 4;

/// Stamp a play or pause glyph in the bottom-right corner.
fn draw_badge(image: &mut image::RgbaImage, badge: Badge) {
	let size = image.width().min(image.height()) as f32;
	let radius = size * BADGE_RADIUS;
	let margin = size * BADGE_MARGIN;
	let cx = image.width() as f32 - margin - radius;
	let cy = image.height() as f32 - margin - radius;

	// The disc first, then the glyph on top of it.
	paint(image, cx, cy, radius, [0, 0, 0], BADGE_OPACITY, &|x, y| {
		let (dx, dy) = (x - cx, y - cy);
		dx * dx + dy * dy <= radius * radius
	});

	let glyph = radius * 0.86;
	match badge {
		Badge::Play => {
			// Nudged right so the triangle looks centred: its visual centre
			// of mass sits left of its bounding box.
			let cx = cx + glyph * 0.08;
			paint(image, cx, cy, radius, [255, 255, 255], 1.0, &|x, y| {
				in_triangle(x, y, cx, cy, glyph)
			});
		}
		Badge::Pause => {
			let bar_w = glyph * 0.28;
			let bar_h = glyph * 0.92;
			let gap = glyph * 0.26;
			paint(image, cx, cy, radius, [255, 255, 255], 1.0, &|x, y| {
				let dy = (y - cy).abs();
				if dy > bar_h / 2.0 {
					return false;
				}
				let dx = (x - cx).abs();
				dx >= gap / 2.0 && dx <= gap / 2.0 + bar_w
			});
		}
	}
}

/// Blend `colour` into the image wherever `inside` reports coverage.
///
/// Only the square around the badge is visited, so this stays cheap however
/// large the cover is.
fn paint(
	image: &mut image::RgbaImage,
	cx: f32,
	cy: f32,
	radius: f32,
	colour: [u8; 3],
	opacity: f32,
	inside: &dyn Fn(f32, f32) -> bool,
) {
	let pad = radius + 2.0;
	let x0 = (cx - pad).floor().max(0.0) as u32;
	let y0 = (cy - pad).floor().max(0.0) as u32;
	let x1 = ((cx + pad).ceil() as u32).min(image.width());
	let y1 = ((cy + pad).ceil() as u32).min(image.height());

	let step = 1.0 / SUPERSAMPLE as f32;
	let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;

	for py in y0..y1 {
		for px in x0..x1 {
			let mut hits = 0u32;
			for sy in 0..SUPERSAMPLE {
				for sx in 0..SUPERSAMPLE {
					let x = px as f32 + (sx as f32 + 0.5) * step;
					let y = py as f32 + (sy as f32 + 0.5) * step;
					if inside(x, y) {
						hits += 1;
					}
				}
			}
			if hits == 0 {
				continue;
			}

			let alpha = (hits as f32 / samples) * opacity;
			let pixel = image.get_pixel_mut(px, py);
			for channel in 0..3 {
				let existing = pixel[channel] as f32;
				pixel[channel] = (existing + (colour[channel] as f32 - existing) * alpha).round() as u8;
			}
		}
	}
}

/// Whether a point falls inside an equilateral "play" triangle pointing right.
fn in_triangle(x: f32, y: f32, cx: f32, cy: f32, size: f32) -> bool {
	let half = size / 2.0;
	let dx = x - (cx - half);
	let dy = y - cy;
	if dx < 0.0 || dx > size {
		return false;
	}
	// The triangle narrows linearly from its flat left edge to the tip.
	let allowed = half * (1.0 - dx / size);
	dy.abs() <= allowed
}
