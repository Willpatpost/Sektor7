//! Renderer configuration and pure selection policies (Stage 2).

use ash::vk;

/// Preferred Vulkan present mode (Stage 2 authorization, item 3).
///
/// The engine's long-term direction is configurable high-refresh
/// presentation (60/144/240+ FPS targets and an uncapped measurement
/// mode); the present-mode choice is one axis of that. `Auto` selects the
/// vsync-on baseline (FIFO — always supported); explicit preferences fall
/// back to FIFO when the device does not expose them, and the resolved
/// mode plus the full supported set are reported in the render stats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PresentModePreference {
    /// FIFO (vsync on) — the guaranteed baseline.
    #[default]
    Auto,
    Fifo,
    /// Triple-buffered low-latency vsync; falls back to FIFO.
    Mailbox,
    /// Tearing-allowed uncapped present; falls back to FIFO.
    Immediate,
}

impl PresentModePreference {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim_ascii().to_ascii_lowercase().as_str() {
            "auto" | "fifo" => Some(PresentModePreference::Auto),
            "mailbox" => Some(PresentModePreference::Mailbox),
            "immediate" => Some(PresentModePreference::Immediate),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PresentModePreference::Auto | PresentModePreference::Fifo => "fifo",
            PresentModePreference::Mailbox => "mailbox",
            PresentModePreference::Immediate => "immediate",
        }
    }
}

/// Validation-layer preference. `Auto` enables validation in debug builds
/// when the layer is installed; the renderer records whether it is
/// actually active so application errors stay distinguishable from
/// environment limitations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ValidationPreference {
    #[default]
    Auto,
    On,
    Off,
}

impl ValidationPreference {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim_ascii().to_ascii_lowercase().as_str() {
            "auto" => Some(ValidationPreference::Auto),
            "on" => Some(ValidationPreference::On),
            "off" => Some(ValidationPreference::Off),
            _ => None,
        }
    }
}

/// Renderer configuration. Presentation *pacing* (frame-rate targets,
/// uncapped measurement) stays in the application — the renderer never
/// imposes an engine-wide frame-rate ceiling, and rendering cadence is
/// deliberately independent of the future fixed-rate campaign simulation
/// (Stage 3, gated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderConfig {
    pub present_mode: PresentModePreference,
    pub validation: ValidationPreference,
    /// Verification scenario (`--replace-pipelines`): force every
    /// swapchain recreation to replace the pipeline state, exercising
    /// the surface-format-change lifecycle — including the dynamic
    /// region-scene buffer carry-over (Stage 4 final-review finding A)
    /// — without a real format change, which resizing alone cannot
    /// produce (the CI resize smoke keeps the surface format). Never
    /// set by normal operation.
    pub replace_pipelines: bool,
}

/// Frames simultaneously in flight (sync objects per slot). Fixed at 2 for
/// Stage 2; the value is a classic double-buffer CPU/GPU overlap.
pub const FRAMES_IN_FLIGHT: u32 = 2;

/// Resolves the present mode against the device's supported set. `FIFO`
/// is guaranteed by the Vulkan spec, so every fallback lands on a valid
/// mode. Pure function — unit-testable without a GPU.
pub(crate) fn resolve_present_mode(
    preference: PresentModePreference,
    supported: &[vk::PresentModeKHR],
) -> vk::PresentModeKHR {
    use vk::PresentModeKHR as Mode;
    let has = |mode: Mode| supported.contains(&mode);
    match preference {
        PresentModePreference::Auto | PresentModePreference::Fifo => Mode::FIFO,
        PresentModePreference::Mailbox => {
            if has(Mode::MAILBOX) {
                Mode::MAILBOX
            } else {
                Mode::FIFO
            }
        }
        PresentModePreference::Immediate => {
            if has(Mode::IMMEDIATE) {
                Mode::IMMEDIATE
            } else {
                Mode::FIFO
            }
        }
    }
}

pub(crate) fn present_mode_str(mode: vk::PresentModeKHR) -> &'static str {
    use vk::PresentModeKHR as Mode;
    match mode {
        Mode::FIFO => "fifo",
        Mode::FIFO_RELAXED => "fifo-relaxed",
        Mode::MAILBOX => "mailbox",
        Mode::IMMEDIATE => "immediate",
        _ => "other",
    }
}

/// The single capacity decision for one dynamic-geometry upload+draw
/// pair (F-03 Step 1, fail-visible over fail-silent): what the frame
/// supplied, the frame slot's vertex capacity, the vertex count
/// actually copied and drawn, and the count omitted.
///
/// Computed ONCE per frame per buffer at the record step — both the
/// upload byte bound (`drawn` × vertex size) and the draw count consume
/// `drawn`, so one physical truncation is counted exactly once (never
/// once per historical clamp site) and the copy can never disagree
/// with the draw: no out-of-bounds copy, no inflated draw count. The
/// omitted count is reported upstream, never fatal — the
/// capacity/refusal policy is Step 2, separately authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DynamicClamp {
    /// Vertices the caller supplied this frame.
    pub supplied: u32,
    /// The frame slot's vertex capacity.
    pub capacity: u32,
    /// Vertices actually copied and drawn (`<= capacity` and
    /// `<= supplied`).
    pub drawn: u32,
    /// Vertices omitted (`supplied − drawn`; 0 in capacity). Raw
    /// vertex counts — an overflow can cut a 36-vertex proxy box
    /// mid-primitive, and the count says so.
    pub truncated: u32,
}

/// The decision is pure and central: `supplied` arrives as the
/// caller's slice length and converts saturating, so a supply beyond
/// `u32::MAX` (unreachable at 24 B/vertex) degrades instead of
/// wrapping — the bound is structural rather than memory luck.
pub(crate) fn dynamic_geometry_clamp(supplied: usize, capacity: u32) -> DynamicClamp {
    let supplied = supplied.min(u32::MAX as usize) as u32;
    DynamicClamp {
        supplied,
        capacity,
        drawn: supplied.min(capacity),
        truncated: supplied.saturating_sub(capacity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supported() -> [vk::PresentModeKHR; 1] {
        [vk::PresentModeKHR::FIFO]
    }

    fn full() -> [vk::PresentModeKHR; 4] {
        [
            vk::PresentModeKHR::FIFO,
            vk::PresentModeKHR::FIFO_RELAXED,
            vk::PresentModeKHR::MAILBOX,
            vk::PresentModeKHR::IMMEDIATE,
        ]
    }

    #[test]
    fn fifo_is_always_selected_for_auto() {
        assert_eq!(
            resolve_present_mode(PresentModePreference::Auto, &full()),
            vk::PresentModeKHR::FIFO
        );
    }

    #[test]
    fn mailbox_prefers_mailbox_when_supported() {
        assert_eq!(
            resolve_present_mode(PresentModePreference::Mailbox, &full()),
            vk::PresentModeKHR::MAILBOX
        );
    }

    #[test]
    fn mailbox_falls_back_to_fifo_when_unsupported() {
        assert_eq!(
            resolve_present_mode(PresentModePreference::Mailbox, &supported()),
            vk::PresentModeKHR::FIFO
        );
    }

    #[test]
    fn immediate_prefers_immediate_when_supported() {
        assert_eq!(
            resolve_present_mode(PresentModePreference::Immediate, &full()),
            vk::PresentModeKHR::IMMEDIATE
        );
    }

    #[test]
    fn immediate_falls_back_to_fifo_when_unsupported() {
        assert_eq!(
            resolve_present_mode(PresentModePreference::Immediate, &supported()),
            vk::PresentModeKHR::FIFO
        );
    }

    #[test]
    fn preference_parsing_round_trips() {
        assert_eq!(
            PresentModePreference::parse("mailbox"),
            Some(PresentModePreference::Mailbox)
        );
        assert_eq!(
            PresentModePreference::parse("AUTO"),
            Some(PresentModePreference::Auto)
        );
        assert_eq!(PresentModePreference::parse("nope"), None);
        assert_eq!(
            ValidationPreference::parse("off"),
            Some(ValidationPreference::Off)
        );
    }

    #[test]
    fn clamp_decision_in_capacity_supplies_every_vertex() {
        // In-capacity geometry is copied and drawn whole — the decision
        // reproduces today's min() values exactly.
        for (supplied, capacity) in [
            (0_usize, 36_800_u32),
            (1, 36_800),
            (36_288, 36_800),
            (299_999, 300_000),
        ] {
            let clamp = dynamic_geometry_clamp(supplied, capacity);
            assert_eq!(clamp.drawn, supplied as u32, "everything fits");
            assert_eq!(clamp.truncated, 0, "nothing omitted");
            assert_eq!(clamp.capacity, capacity);
        }
    }

    #[test]
    fn clamp_decision_at_capacity_boundary_omits_nothing() {
        let clamp = dynamic_geometry_clamp(36_800, 36_800);
        assert_eq!(clamp.drawn, 36_800);
        assert_eq!(clamp.truncated, 0);
    }

    #[test]
    fn clamp_decision_oversize_omits_exact_delta() {
        // The capacity-decision logic with oversized synthetic input —
        // exact deltas, including the smoke's mod-36 derivation
        // (push_box emits 36 verts/call; the region capacity 36,800 is
        // ≡ 8 mod 36, so a 1,050-box supply of 37,800 omits exactly
        // 1,000) and the overlay case (2,000 'A' glyphs × 168 verts =
        // 336,000 vs 300,000 → 36,000).
        for (supplied, capacity, omitted) in [
            (36_801_usize, 36_800_u32, 1_u32),
            (36_836, 36_800, 36),
            (37_800, 36_800, 1_000),
            (336_000, 300_000, 36_000),
        ] {
            let clamp = dynamic_geometry_clamp(supplied, capacity);
            assert_eq!(clamp.drawn, capacity, "the draw never exceeds capacity");
            assert_eq!(clamp.truncated, omitted, "the exact omitted delta");
            assert_eq!(
                clamp.supplied,
                clamp
                    .drawn
                    .checked_add(clamp.truncated)
                    .expect("drawn + omitted == supplied by construction"),
                "supplied is exactly drawn + omitted"
            );
        }
        // A supply beyond u32::MAX (unreachable at 24 B/vertex)
        // saturates instead of wrapping.
        let huge = dynamic_geometry_clamp(usize::MAX, 36_800);
        assert_eq!(huge.supplied, u32::MAX);
        assert_eq!(huge.drawn, 36_800);
    }

    #[test]
    fn clamp_decision_byte_bound_matches_vertex_clamp() {
        // The upload/draw agreement — no out-of-bounds copy, no
        // inflated draw — asserted per bound for BOTH dynamic vertex
        // types: the copy length is drawn × vertex size, which never
        // exceeds the slot's byte capacity nor the supplied bytes.
        use crate::overlay::OverlayVertex;
        use crate::scene::SceneVertex;

        assert_eq!(std::mem::size_of::<SceneVertex>(), 24);
        assert_eq!(std::mem::size_of::<OverlayVertex>(), 24);
        for (supplied, capacity) in [
            (36_288_usize, 36_800_u32),
            (37_800, 36_800),
            (336_000, 300_000),
            (700_000, 300_000),
        ] {
            let clamp = dynamic_geometry_clamp(supplied, capacity);
            assert!(
                clamp.drawn <= clamp.capacity,
                "draw never exceeds the per-slot capacity"
            );
            let slot_bytes = capacity as usize * 24;
            assert!(
                clamp.drawn as usize * 24 <= slot_bytes,
                "copy bytes fit the slot"
            );
            assert!(
                clamp.drawn as usize * 24 <= supplied * 24,
                "copy bytes never exceed the supplied bytes"
            );
        }
    }

    #[test]
    fn clamp_decision_degenerate_capacities_refuse_whole() {
        // Zero supply draws nothing; a zero capacity (an unallocated
        // buffer) would omit everything — defensive only: the record
        // path never reaches a non-empty supply with capacity 0 (the
        // lazy region buffer allocates first).
        let empty = dynamic_geometry_clamp(0, 36_800);
        assert_eq!((empty.drawn, empty.truncated), (0, 0));
        let unallocated = dynamic_geometry_clamp(1_000, 0);
        assert_eq!((unallocated.drawn, unallocated.truncated), (0, 1_000));
    }
}
