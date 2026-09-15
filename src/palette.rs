//! Picking a key colour out of cover art.
//!
//! The goal is a colour that *evokes* the sleeve, not its average — averaging
//! a whole cover reliably produces the same muddy brown for every album. So
//! this looks for the dominant **vibrant** hue, ignoring the greys, blacks and
//! whites that dominate most images by pixel count but say nothing about how
//! a cover feels.
//!
//! Whatever comes out is then forced to be legible: keys carry white glyphs,
//! so the returned colour is guaranteed to clear WCAG AA contrast against
//! white. A bright yellow sleeve yields a deep ochre, not an unreadable key.

use image::{DynamicImage, RgbaImage};

/// Working size for the scan. Large enough to keep a small but saturated
/// element (a logo, a jacket) represented, small enough that the scan is
/// trivial next to decoding the image in the first place.
const SCAN_SIZE: u32 = 64;

/// Hue buckets, 15 degrees each.
const BUCKETS: usize = 24;

/// Pixels outside this lightness band are backdrop, not colour.
const MIN_LIGHTNESS: f32 = 0.12;
const MAX_LIGHTNESS: f32 = 0.92;

/// Below this saturation a pixel is grey as far as hue is concerned.
const MIN_SATURATION: f32 = 0.15;

/// Fraction of scanned pixels that must carry colour before the result is
/// trusted. Under it the cover is treated as monochrome.
const MIN_COLOURED_FRACTION: f32 = 0.02;

/// Used for covers with no usable colour — a monochrome sleeve should look
/// deliberate, not like a random hue was invented for it.
const NEUTRAL: [u8; 3] = [0x2A, 0x2E, 0x35];

/// Lightness band a key colour is forced into, before the contrast check.
const TARGET_MIN_L: f32 = 0.20;
const TARGET_MAX_L: f32 = 0.34;

/// Saturation ceiling, so a fiercely saturated sleeve does not produce a key
/// that vibrates next to its white glyph.
const MAX_SATURATION: f32 = 0.72;

/// Minimum contrast against white. WCAG AA for large text and UI components.
const MIN_CONTRAST: f32 = 4.5;

/// The key colour for a cover.
pub fn accent(image: &DynamicImage) -> [u8; 3] {
	let scan = image
		.resize_exact(SCAN_SIZE, SCAN_SIZE, image::imageops::FilterType::Triangle)
		.to_rgba8();
	// The neutral is already chosen to be readable, so it is returned as-is
	// rather than pushed through the correction, which would shift it.
	dominant_hue_colour(&scan).map_or(NEUTRAL, readable)
}

/// Average colour of the heaviest hue bucket, or `None` when the cover
/// carries no usable colour.
fn dominant_hue_colour(scan: &RgbaImage) -> Option<[u8; 3]> {
	// Per bucket: accumulated weight, then weighted sums of each channel.
	let mut weights = [0f32; BUCKETS];
	let mut sums = [[0f32; 3]; BUCKETS];
	let mut coloured = 0f32;

	for pixel in scan.pixels() {
		// A transparent pixel contributes nothing; covers are opaque but
		// local artwork need not be.
		if pixel[3] < 128 {
			continue;
		}
		let rgb = [pixel[0], pixel[1], pixel[2]];
		let (hue, saturation, lightness) = to_hsl(rgb);

		if saturation < MIN_SATURATION || !(MIN_LIGHTNESS..=MAX_LIGHTNESS).contains(&lightness) {
			continue;
		}

		// Saturation squared so a small vivid area outweighs a large washed
		// out one; the lightness term discounts colours near the extremes,
		// which read as tints of white or black rather than as a hue.
		let weight = saturation * saturation * (1.0 - (lightness - 0.5).abs());
		let bucket = ((hue / 360.0) * BUCKETS as f32) as usize % BUCKETS;

		weights[bucket] += weight;
		for channel in 0..3 {
			sums[bucket][channel] += rgb[channel] as f32 * weight;
		}
		coloured += 1.0;
	}

	if coloured < (scan.width() * scan.height()) as f32 * MIN_COLOURED_FRACTION {
		return None;
	}

	let (best, weight) = weights
		.iter()
		.enumerate()
		.max_by(|a, b| a.1.total_cmp(b.1))
		.map(|(index, weight)| (index, *weight))
		.unwrap_or((0, 0.0));

	if weight <= f32::EPSILON {
		return None;
	}

	Some([
		(sums[best][0] / weight).round() as u8,
		(sums[best][1] / weight).round() as u8,
		(sums[best][2] / weight).round() as u8,
	])
}

/// Force a colour into a band where a white glyph stays readable on it.
fn readable(rgb: [u8; 3]) -> [u8; 3] {
	let (hue, saturation, lightness) = to_hsl(rgb);
	let saturation = saturation.min(MAX_SATURATION);
	let mut lightness = lightness.clamp(TARGET_MIN_L, TARGET_MAX_L);

	// The band alone is not enough: at equal lightness a yellow is far
	// brighter to the eye than a blue, so darken until the measured contrast
	// clears the bar rather than trusting the band.
	let mut candidate = from_hsl(hue, saturation, lightness);
	while contrast_against_white(candidate) < MIN_CONTRAST && lightness > 0.04 {
		lightness -= 0.01;
		candidate = from_hsl(hue, saturation, lightness);
	}
	candidate
}

/// WCAG contrast ratio between a colour and white.
fn contrast_against_white(rgb: [u8; 3]) -> f32 {
	let luminance = relative_luminance(rgb);
	// White's relative luminance is 1.0.
	(1.0 + 0.05) / (luminance + 0.05)
}

fn relative_luminance(rgb: [u8; 3]) -> f32 {
	fn channel(value: u8) -> f32 {
		let v = value as f32 / 255.0;
		if v <= 0.04045 {
			v / 12.92
		} else {
			((v + 0.055) / 1.055).powf(2.4)
		}
	}
	0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

/// Hue in degrees, saturation and lightness in 0..1.
fn to_hsl(rgb: [u8; 3]) -> (f32, f32, f32) {
	let (r, g, b) = (
		rgb[0] as f32 / 255.0,
		rgb[1] as f32 / 255.0,
		rgb[2] as f32 / 255.0,
	);
	let max = r.max(g).max(b);
	let min = r.min(g).min(b);
	let lightness = (max + min) / 2.0;
	let delta = max - min;

	if delta <= f32::EPSILON {
		return (0.0, 0.0, lightness);
	}

	let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs()).max(f32::EPSILON);
	let hue = if max == r {
		60.0 * (((g - b) / delta) % 6.0)
	} else if max == g {
		60.0 * ((b - r) / delta + 2.0)
	} else {
		60.0 * ((r - g) / delta + 4.0)
	};

	((hue + 360.0) % 360.0, saturation.clamp(0.0, 1.0), lightness)
}

fn from_hsl(hue: f32, saturation: f32, lightness: f32) -> [u8; 3] {
	let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
	let h = (hue % 360.0) / 60.0;
	let x = c * (1.0 - (h % 2.0 - 1.0).abs());
	let (r, g, b) = match h as u32 {
		0 => (c, x, 0.0),
		1 => (x, c, 0.0),
		2 => (0.0, c, x),
		3 => (0.0, x, c),
		4 => (x, 0.0, c),
		_ => (c, 0.0, x),
	};
	let m = lightness - c / 2.0;
	[
		(((r + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
		(((g + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
		(((b + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
	]
}

#[cfg(test)]
mod tests {
	use image::{DynamicImage, Rgba, RgbaImage};

	use super::*;

	/// A cover that is `colour` over `fraction` of its area, grey elsewhere.
	fn cover(colour: [u8; 3], fraction: f32) -> DynamicImage {
		let mut image = RgbaImage::from_pixel(128, 128, Rgba([120, 120, 120, 255]));
		let rows = (128.0 * fraction) as u32;
		for y in 0..rows {
			for x in 0..128 {
				image.put_pixel(x, y, Rgba([colour[0], colour[1], colour[2], 255]));
			}
		}
		DynamicImage::ImageRgba8(image)
	}

	fn hue_of(rgb: [u8; 3]) -> f32 {
		to_hsl(rgb).0
	}

	/// Hues wrap, so "is this red" cannot be a plain range check.
	fn hue_distance(a: f32, b: f32) -> f32 {
		let diff = (a - b).abs() % 360.0;
		diff.min(360.0 - diff)
	}

	#[test]
	fn a_red_cover_yields_a_red_key() {
		let accent = accent(&cover([220, 30, 40], 0.5));
		assert!(
			hue_distance(hue_of(accent), 0.0) < 25.0,
			"expected a red hue, got {:?} at {}deg",
			accent,
			hue_of(accent)
		);
	}

	#[test]
	fn a_teal_cover_yields_a_teal_key() {
		let accent = accent(&cover([20, 180, 170], 0.5));
		assert!(
			hue_distance(hue_of(accent), 175.0) < 25.0,
			"expected a teal hue, got {:?} at {}deg",
			accent,
			hue_of(accent)
		);
	}

	/// Averaging the whole image would return grey here. The point of hue
	/// bucketing is that a minority of saturated pixels still decides.
	#[test]
	fn a_small_vivid_area_still_decides_the_hue() {
		let accent = accent(&cover([240, 120, 0], 0.12));
		assert!(
			hue_distance(hue_of(accent), 30.0) < 30.0,
			"expected an orange hue, got {:?} at {}deg",
			accent,
			hue_of(accent)
		);
	}

	#[test]
	fn a_monochrome_cover_falls_back_to_neutral() {
		let mut image = RgbaImage::from_pixel(128, 128, Rgba([200, 200, 200, 255]));
		for y in 0..64 {
			for x in 0..128 {
				image.put_pixel(x, y, Rgba([40, 40, 40, 255]));
			}
		}
		assert_eq!(accent(&DynamicImage::ImageRgba8(image)), NEUTRAL);
	}

	/// The guarantee the whole module exists to make: keys carry white
	/// glyphs, so no cover may produce a colour they cannot be read on.
	#[test]
	fn every_hue_produces_a_key_readable_under_white() {
		for degrees in (0..360).step_by(5) {
			for lightness in [0.15, 0.35, 0.55, 0.75, 0.95] {
				let source = from_hsl(degrees as f32, 1.0, lightness);
				let key = accent(&cover(source, 0.6));
				let contrast = contrast_against_white(key);
				assert!(
					contrast >= MIN_CONTRAST,
					"hue {degrees} at l={lightness} gave {key:?}, contrast {contrast:.2}"
				);
			}
		}
	}

	/// Yellow is the hard case: it clears the lightness band easily but is
	/// far brighter to the eye than any other hue at the same lightness.
	#[test]
	fn a_bright_yellow_cover_is_darkened_until_it_is_readable() {
		let key = accent(&cover([255, 235, 0], 0.7));
		assert!(contrast_against_white(key) >= MIN_CONTRAST);
		assert!(
			hue_distance(hue_of(key), 55.0) < 25.0,
			"darkening should keep the hue, got {key:?}"
		);
	}

	#[test]
	fn contrast_matches_the_wcag_reference_values() {
		// Black on white is the maximum possible ratio, 21:1.
		assert!((contrast_against_white([0, 0, 0]) - 21.0).abs() < 0.05);
		assert!((contrast_against_white([255, 255, 255]) - 1.0).abs() < 0.01);
	}

	#[test]
	fn hsl_survives_a_round_trip() {
		for rgb in [[220, 30, 40], [20, 180, 170], [255, 235, 0], [90, 90, 90]] {
			let (h, s, l) = to_hsl(rgb);
			let back = from_hsl(h, s, l);
			for channel in 0..3 {
				assert!(
					(back[channel] as i16 - rgb[channel] as i16).abs() <= 2,
					"{rgb:?} round-tripped to {back:?}"
				);
			}
		}
	}
}
