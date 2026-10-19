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
    let (width, height) = (extent.0.max(1) as f32, extent.1.max(1) as f32);
    let mut vertices = Vec::new();
    let mut glyph_x = 0.0f32;
    let mut glyph_y = 4.0f32;
    let mut glyph_count = 0usize;

    for ch in text.chars() {
        if ch == '\n' {
            glyph_x = 0.0;
            glyph_y += 8.0 * SCALE + 4.0;
            continue;
        }
        let ascii = if ch.is_ascii() { ch as u8 } else { b'?' };
        let glyph = font8x8::legacy::BASIC_LEGACY[ascii as usize];
        for (row, row_bits) in glyph.iter().enumerate() {
            for bit in 0..8 {
                if row_bits & (1 << bit) == 0 {
                    continue;
                }
                // Pixel-space rect → NDC (x right, y down in Vulkan).
                let px = glyph_x + bit as f32 * SCALE;
                let py = glyph_y + row as f32 * SCALE;
                let x0 = px / width * 2.0 - 1.0;
                let x1 = (px + SCALE) / width * 2.0 - 1.0;
                let y0 = 1.0 - py / height * 2.0;
                let y1 = 1.0 - (py + SCALE) / height * 2.0;
                let quad = [[x0, y0], [x1, y0], [x1, y1], [x0, y0], [x1, y1], [x0, y1]];
                for pos in quad {
                    vertices.push(OverlayVertex { pos, col: color });
                }
            }
        }
        glyph_x += 8.0 * SCALE + 2.0;
        glyph_count += 1;
        if glyph_count >= MAX_GLYPHS {
            break;
        }
    }
    vertices
}

#[cfg(test)]
mod tests {
    use super::*;

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
