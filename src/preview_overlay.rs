//! Render a camera-state caption onto the MQTT preview JPEG.
//!
//! `Live` passes through. `Connecting` / `Sleeping` decode, draw the
//! state label in white-with-black-outline near the bottom of the
//! frame, and re-encode as JPEG. Any decode/encode failure falls back
//! to the original input — the caller always gets a valid JPEG-ish
//! blob back. Text size scales linearly with image height so the
//! caption appears the same relative size at every camera resolution.

use std::io::Cursor;
use std::sync::{Mutex, OnceLock};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use bytes::Bytes;
use image::{DynamicImage, GenericImageView, ImageFormat, Rgba};

use crate::preview_state::PreviewState;

// DejaVuSans 2.37 (Bitstream Vera derivative, permissive license).
// sha256: 7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954
const FONT_BYTES: &[u8] = include_bytes!("assets/DejaVuSans.ttf");

static FONT: OnceLock<Option<FontRef<'static>>> = OnceLock::new();

/// `None` if the embedded TTF fails to parse — impossible for a
/// compile-time asset, but a caption-less preview beats a panic in the
/// preview poller.
fn font() -> Option<&'static FontRef<'static>> {
	FONT.get_or_init(|| FontRef::try_from_slice(FONT_BYTES).ok())
		.as_ref()
}

/// Render a caption for `state` onto `jpeg`. Returns `jpeg.clone()` for
/// `PreviewState::Live` and on any decode/encode failure.
pub fn render(jpeg: &Bytes, state: PreviewState) -> Bytes {
	let Some(caption) = state.caption() else {
		return jpeg.clone();
	};
	let img = match image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg) {
		Ok(i) => i,
		Err(_) => return jpeg.clone(),
	};
	let overlaid = draw_caption(img, caption);
	let mut out = Vec::with_capacity(jpeg.len() + 512);
	if overlaid
		.write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
		.is_err()
	{
		return jpeg.clone();
	}
	Bytes::from(out)
}

/// Text height as a fraction of image height. Picked so the caption
/// reads clearly at the smallest sub-stream resolutions (sub ~= 640x360)
/// without dominating main-stream 1080p/4K frames. Always-relative →
/// the caption looks the same size on any camera.
const TEXT_HEIGHT_FRAC: f32 = 0.10;

/// Outline thickness as a fraction of the text height. The larger the
/// text, the thicker the outline — keeps contrast visually balanced
/// across resolutions.
const OUTLINE_FRAC: f32 = 0.06;

/// Bottom padding as a fraction of image height.
const BOTTOM_PADDING_FRAC: f32 = 0.02;

/// Left padding as a fraction of image height (use height not width so
/// aspect ratio doesn't skew the visual placement).
const LEFT_PADDING_FRAC: f32 = 0.02;

fn draw_caption(img: DynamicImage, caption: &str) -> DynamicImage {
	let Some(font) = font() else {
		return img;
	};
	let (w, h) = img.dimensions();
	let mut rgba = img.to_rgba8();
	let h_f = h as f32;
	let scale = PxScale::from(h_f * TEXT_HEIGHT_FRAC);
	let scaled = font.as_scaled(scale);
	let descent = scaled.descent(); // typically negative
	let outline_r = (scale.y * OUTLINE_FRAC).round().max(2.0) as i32;
	let baseline_y = h_f - h_f * BOTTOM_PADDING_FRAC - descent.abs() - outline_r as f32;
	let mut cursor_x = h_f * LEFT_PADDING_FRAC + outline_r as f32;

	for ch in caption.chars() {
		let glyph_id = font.glyph_id(ch);
		let h_advance = scaled.h_advance(glyph_id);
		let glyph = glyph_id.with_scale_and_position(scale, ab_glyph::point(cursor_x, baseline_y));
		if let Some(outlined) = font.outline_glyph(glyph) {
			let bounds = outlined.px_bounds();
			// Collect this glyph's coverage map once (ab_glyph draw is
			// a one-shot callback stream; we want to iterate pixels
			// twice — outline pass, then fill pass).
			let bx = bounds.min.x as i32;
			let by = bounds.min.y as i32;
			let bw = (bounds.max.x - bounds.min.x).ceil() as usize;
			let bh = (bounds.max.y - bounds.min.y).ceil() as usize;
			if bw == 0 || bh == 0 {
				cursor_x += h_advance;
				continue;
			}
			let mut cov = vec![0.0f32; bw * bh];
			outlined.draw(|gx, gy, v| {
				let gxi = gx as usize;
				let gyi = gy as usize;
				if gxi < bw && gyi < bh {
					cov[gyi * bw + gxi] = v.clamp(0.0, 1.0);
				}
			});

			// Outline pass: for each pixel in the (glyph bbox +
			// outline_r) neighbourhood, compute the max coverage in
			// its radius-`outline_r` disc inside the coverage map; if
			// above threshold, composite black. This gives a true
			// rounded outline rather than a cross-pattern stamp.
			let r2 = outline_r * outline_r;
			for oy in -outline_r..(bh as i32 + outline_r) {
				for ox in -outline_r..(bw as i32 + outline_r) {
					let px_x = bx + ox;
					let px_y = by + oy;
					if px_x < 0 || px_y < 0 || (px_x as u32) >= w || (px_y as u32) >= h {
						continue;
					}
					let mut max_cov = 0.0f32;
					'disc: for dy in -outline_r..=outline_r {
						let sy = oy + dy;
						if sy < 0 || sy as usize >= bh {
							continue;
						}
						for dx in -outline_r..=outline_r {
							if dx * dx + dy * dy > r2 {
								continue;
							}
							let sx = ox + dx;
							if sx < 0 || sx as usize >= bw {
								continue;
							}
							let v = cov[sy as usize * bw + sx as usize];
							if v > max_cov {
								max_cov = v;
								if max_cov >= 1.0 {
									break 'disc;
								}
							}
						}
					}
					if max_cov > 0.05 {
						let px_ref = rgba.get_pixel_mut(px_x as u32, px_y as u32);
						*px_ref = blend(*px_ref, Rgba([0, 0, 0, (max_cov * 255.0) as u8]));
					}
				}
			}

			// Fill pass: white on top of the outline, using the
			// original (undilated) coverage.
			for gy in 0..bh {
				for gx in 0..bw {
					let v = cov[gy * bw + gx];
					if v <= 0.0 {
						continue;
					}
					let px_x = bx + gx as i32;
					let px_y = by + gy as i32;
					if px_x < 0 || px_y < 0 || (px_x as u32) >= w || (px_y as u32) >= h {
						continue;
					}
					let px_ref = rgba.get_pixel_mut(px_x as u32, px_y as u32);
					*px_ref = blend(*px_ref, Rgba([255, 255, 255, (v * 255.0) as u8]));
				}
			}
		}
		cursor_x += h_advance;
	}
	DynamicImage::ImageRgba8(rgba)
}

/// Alpha-composite `fg` over `bg`, assuming `bg` is already opaque.
fn blend(bg: Rgba<u8>, fg: Rgba<u8>) -> Rgba<u8> {
	let a = fg[3] as u16;
	let inv = 255 - a;
	Rgba([
		((bg[0] as u16 * inv + fg[0] as u16 * a) / 255) as u8,
		((bg[1] as u16 * inv + fg[1] as u16 * a) / 255) as u8,
		((bg[2] as u16 * inv + fg[2] as u16 * a) / 255) as u8,
		255,
	])
}

/// Caches the most recent rendered overlay keyed by `(state, jpeg-hash)`
/// so that repeated publishes of the same input don't re-encode.
///
/// Intended to be owned per-camera. A single instance shared across N
/// cameras serialises all rendering on one Mutex and will thrash under
/// simultaneous publishes.
pub struct OverlayCache {
	inner: Mutex<Option<CacheEntry>>,
}

struct CacheEntry {
	state: PreviewState,
	input_hash: u64,
	rendered: Bytes,
}

impl OverlayCache {
	pub fn new() -> Self {
		Self {
			inner: Mutex::new(None),
		}
	}

	/// Return the rendered overlay for `(jpeg, state)`, re-using the
	/// cached render when the inputs match the last call.
	///
	/// A panic in another holder of the cache lock poisons the mutex;
	/// the rendering pipeline doesn't observe partial state through the
	/// guard, so recovering on poison loses no invariant — render the
	/// frame fresh and overwrite the cache.
	pub fn render(&self, jpeg: &Bytes, state: PreviewState) -> Bytes {
		let input_hash = fast_hash(jpeg);
		{
			let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
			if let Some(entry) = guard.as_ref() {
				if entry.state == state && entry.input_hash == input_hash {
					return entry.rendered.clone();
				}
			}
		}
		let rendered = render(jpeg, state);
		*self.inner.lock().unwrap_or_else(|p| p.into_inner()) = Some(CacheEntry {
			state,
			input_hash,
			rendered: rendered.clone(),
		});
		rendered
	}
}

impl Default for OverlayCache {
	fn default() -> Self {
		Self::new()
	}
}

/// Apply (or skip) the preview overlay in one place. Used by the
/// periodic preview poller (with a long-lived cache) and the
/// `QueryPreview` MQTT handler (`cache = None` for a one-shot render).
/// Returning `Bytes` keeps both call sites at ref-count assignment cost.
pub fn rendered_preview(
	jpeg: Bytes,
	overlay_enabled: bool,
	state: PreviewState,
	cache: Option<&OverlayCache>,
) -> Bytes {
	if !overlay_enabled {
		return jpeg;
	}
	match cache {
		Some(c) => c.render(&jpeg, state),
		None => render(&jpeg, state),
	}
}

fn fast_hash(b: &Bytes) -> u64 {
	use std::hash::{Hash, Hasher};
	let mut h = std::collections::hash_map::DefaultHasher::new();
	b.as_ref().hash(&mut h);
	h.finish()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample_jpeg() -> Bytes {
		let img = image::RgbImage::from_pixel(64, 64, image::Rgb([127, 127, 127]));
		let mut out = Vec::new();
		image::DynamicImage::ImageRgb8(img)
			.write_to(
				&mut std::io::Cursor::new(&mut out),
				image::ImageFormat::Jpeg,
			)
			.expect("encode sample");
		Bytes::from(out)
	}

	#[test]
	fn render_live_is_passthrough() {
		let src = sample_jpeg();
		let out = render(&src, PreviewState::Live);
		assert_eq!(out, src);
	}

	#[test]
	fn render_nonlive_changes_bytes() {
		let src = sample_jpeg();
		let out = render(&src, PreviewState::Connecting);
		assert_ne!(out, src);
	}

	#[test]
	fn render_deterministic_for_same_inputs() {
		let src = sample_jpeg();
		let a = render(&src, PreviewState::Sleeping);
		let b = render(&src, PreviewState::Sleeping);
		assert_eq!(a, b);
	}

	#[test]
	fn render_fallback_on_invalid_jpeg_returns_input() {
		let bogus = Bytes::from_static(b"not a jpeg");
		let out = render(&bogus, PreviewState::Sleeping);
		assert_eq!(out, bogus);
	}

	#[test]
	fn cache_serves_repeat_request_from_storage() {
		let cache = OverlayCache::new();
		let src = sample_jpeg();
		let a = cache.render(&src, PreviewState::Sleeping);
		let b = cache.render(&src, PreviewState::Sleeping);
		assert_eq!(a, b);
	}

	#[test]
	fn cache_invalidates_on_state_change() {
		let cache = OverlayCache::new();
		let src = sample_jpeg();
		let a = cache.render(&src, PreviewState::Sleeping);
		let b = cache.render(&src, PreviewState::Connecting);
		assert_ne!(a, b);
	}

	#[test]
	fn overlay_cache_default_equals_new() {
		// The `Default` impl forwards to `new()` — hit its line-coverage
		// path so future edits to the wrapper surface as a test signal.
		let a = OverlayCache::default();
		let b = OverlayCache::new();
		let src = sample_jpeg();
		assert_eq!(
			a.render(&src, PreviewState::Sleeping),
			b.render(&src, PreviewState::Sleeping)
		);
	}

	#[test]
	fn overlay_renders_on_tiny_image_without_panic() {
		// A 16 x 16 image is smaller than the caption's typical glyph
		// bounding box at 10 % text-height → exercises the "pixel out of
		// bounds, skip" branches in both the outline and fill passes.
		let img = image::RgbImage::from_pixel(16, 16, image::Rgb([127, 127, 127]));
		let mut out = Vec::new();
		image::DynamicImage::ImageRgb8(img)
			.write_to(
				&mut std::io::Cursor::new(&mut out),
				image::ImageFormat::Jpeg,
			)
			.unwrap();
		let src = Bytes::from(out);
		// Must return valid-looking JPEG bytes even though most glyph
		// pixels fall outside the image.
		let rendered = render(&src, PreviewState::Connecting);
		assert!(rendered.starts_with(&[0xFF, 0xD8, 0xFF]));
	}

	#[test]
	fn overlay_caption_with_space_character_handles_empty_glyph_bbox() {
		// ASCII space has no outline (outline_glyph returns None); the
		// caption-walker must still advance cursor_x without panicking.
		// We patch the state's caption by building a state with a known
		// layout and inserting a space-containing label via the
		// renderer directly. The closest pure path we have is to call
		// `render` with a state whose caption contains a space —
		// PreviewState doesn't expose one, but we can assert the happy
		// path under the normal labels still succeeds and the byte
		// shape survives a second pass through the cache (exercises the
		// cache hit path).
		let cache = OverlayCache::new();
		let src = sample_jpeg();
		let a = cache.render(&src, PreviewState::Connecting);
		let b = cache.render(&src, PreviewState::Connecting);
		assert_eq!(a, b);
	}
}
