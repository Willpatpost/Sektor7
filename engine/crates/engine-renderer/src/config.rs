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
}
