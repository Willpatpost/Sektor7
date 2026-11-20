//! Debug-overlay glyph quad building (Stage 2 authorization, item 4).
//!
//! Minimal by design: the public-domain font8x8 bitmap font rendered as
//! solid alpha-blended quads — one small quad per set pixel — so the
//! overlay needs no glyph texture, sampler, or descriptor infrastructure.
//! No UI framework.

/// Overlay vertex: NDC position + RGBA color (24 bytes, matches
/// `overlay.vert`).
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct OverlayVertex {
    pub pos: [f32; 2],
    pub col: [f32; 4],
}

/// Pixels per glyph axis (font8x8 is 8×8), scaled 2×.
const SCALE: f32 = 2.0;
/// Maximum glyphs buffered (guard for the persistently mapped vertex
/// buffer; ~6 KB of text).
const MAX_GLYPHS: usize = 6_000;
/// Overlay vertex-buffer capacity: generous worst-case glyph density
/// (600k vertices ≈ 14 MB host-visible memory; typical stats text uses
/// ~1–2% of it).
pub fn overlay_capacity_vertices() -> u32 {
    600_000
}

/// Builds overlay vertices for `text` (newlines advance rows) in NDC for
/// the given swapchain extent. Glyph pixels come from font8x8's
/// `BASIC_LEGACY` (bit 0 of each row byte is the leftmost pixel).
pub fn build_overlay_vertices(
    text: &str,
    extent: (u32, u32),
    color: [f32; 4],
) -> Vec<OverlayVertex> {
    let mut vertices = Vec::new();
    build_overlay_vertices_into(&mut vertices, text, extent, color);
    vertices
}

/// The caller-held scratch form (the F-05 measured candidate): clears
/// and refills the caller's buffer — the capacity retained from the
/// first frames is reused every frame (no per-frame allocation or
/// growth); the vertex content is identical to the allocating form.
pub fn build_overlay_vertices_into(
    vertices: &mut Vec<OverlayVertex>,
    text: &str,
    extent: (u32, u32),
    color: [f32; 4],
) {
    vertices.clear();
    push_text_px(vertices, extent, [0.0, 4.0], SCALE, text, color);
}

/// Appends a solid RGBA rectangle, specified in pixels from the top left.
pub fn push_rect_px(
    vertices: &mut Vec<OverlayVertex>,
    extent: (u32, u32),
    rect: [f32; 4],
    color: [f32; 4],
) {
    let [x, y, width, height] = rect;
    if width > 0.0 && height > 0.0 && color.iter().all(|value| value.is_finite()) {
        append_quad(vertices, extent, [x, y, x + width, y + height], color);
    }
}

#[inline]
fn append_quad(
    vertices: &mut Vec<OverlayVertex>,
    extent: (u32, u32),
    bounds: [f32; 4],
    color: [f32; 4],
) {
    let [left, top, right, bottom] = bounds;
    let (width, height) = (extent.0.max(1) as f32, extent.1.max(1) as f32);
    let x0 = left / width * 2.0 - 1.0;
    let x1 = right / width * 2.0 - 1.0;
    let y0 = 1.0 - top / height * 2.0;
    let y1 = 1.0 - bottom / height * 2.0;
    if ![x0, y0, x1, y1].into_iter().all(f32::is_finite) {
        return;
    }
    for pos in [[x0, y0], [x1, y0], [x1, y1], [x0, y0], [x1, y1], [x0, y1]] {
        vertices.push(OverlayVertex { pos, col: color });
    }
}

/// Appends bitmap text at a pixel-space origin, with each font texel
/// covering `scale` pixels on each axis. Newlines return to that origin.
pub fn push_text_px(
    vertices: &mut Vec<OverlayVertex>,
    extent: (u32, u32),
    origin: [f32; 2],
    scale: f32,
    text: &str,
    color: [f32; 4],
) {
    if !(scale > 0.0 && scale.is_finite() && origin.iter().chain(&color).all(|v| v.is_finite())) {
        return;
    }
    let [mut glyph_x, mut glyph_y] = origin;
    let mut glyph_count = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            glyph_x = origin[0];
            glyph_y += 10.0 * scale;
            continue;
        }
        let ascii = if ch.is_ascii() { ch as u8 } else { b'?' };
        let glyph = font8x8::legacy::BASIC_LEGACY[ascii as usize];
        for (row, row_bits) in glyph.iter().enumerate() {
            for bit in 0..8 {
                if row_bits & (1 << bit) != 0 {
                    let x = glyph_x + bit as f32 * scale;
                    let y = glyph_y + row as f32 * scale;
                    append_quad(vertices, extent, [x, y, x + scale, y + scale], color);
                }
            }
        }
        glyph_x += 9.0 * scale;
        glyph_count += 1;
        if glyph_count >= MAX_GLYPHS {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positioned_rectangles_and_text_share_resized_pixel_coordinates() {
        let color = [0.25, 0.5, 0.75, 1.0];
        let mut overflow = Vec::new();
        push_rect_px(&mut overflow, (1, 1), [f32::MAX, 0.0, f32::MAX, 1.0], color);
        push_text_px(
            &mut overflow,
            (1, 1),
            [f32::MAX, f32::MAX],
            f32::MAX,
            "X",
            color,
        );
        assert!(overflow.is_empty());
        for (width, height) in [(1280, 720), (641, 479)] {
            let extent = (width, height);
            let (x, y) = (width as f32 / 2.0, height as f32 / 2.0);
            let mut vertices = Vec::new();
            push_rect_px(&mut vertices, extent, [x, y, 8.0, 4.0], color);
            assert_eq!(vertices.len(), 6);
            assert_eq!(vertices[0].pos, [0.0, 0.0]);
            let [right, bottom] = vertices[2].pos;
            assert!((right * x - 8.0).abs() < 0.0001);
            assert!((bottom * y + 4.0).abs() < 0.0001);
            push_text_px(&mut vertices, extent, [x, y], 1.0, "A\nA", color);
            assert_eq!(vertices.len(), 6 + 2 * 28 * 6);
            let [x0, y0] = vertices[6].pos;
            let [x1, y1] = vertices[6 + 28 * 6].pos;
            assert_eq!(x0, x1);
            assert!(((y0 - y1) * y - 10.0).abs() < 0.0001);
            assert!(vertices.iter().all(|v| v.col == color));
        }
    }

    #[test]
    fn known_glyph_produces_expected_quads() {
        // 'A' in font8x8 basic: rows 0x18, 0x3C, 0x66, 0x66, 0x7E, 0x66,
        // 0x66, 0x00 → bit counts 2+4+4+4+6+4+4 = 28 set pixels.
        let verts = build_overlay_vertices("A", (800, 600), [1.0; 4]);
        assert_eq!(verts.len(), 28 * 6, "six vertices per set pixel");
        // All positions inside NDC.
        for v in &verts {
            assert!(v.pos[0] >= -1.0 && v.pos[0] <= 1.0);
            assert!(v.pos[1] >= -1.0 && v.pos[1] <= 1.0);
        }
    }

    #[test]
    fn newline_advances_rows_and_cap_holds() {
        let two_lines = build_overlay_vertices("A\nA", (800, 600), [1.0; 4]);
        assert_eq!(two_lines.len(), 2 * 28 * 6);
        let capped = build_overlay_vertices(&"A".repeat(10_000), (800, 600), [1.0; 4]);
        assert_eq!(capped.len(), MAX_GLYPHS * 28 * 6, "glyph cap enforced");
    }
}
