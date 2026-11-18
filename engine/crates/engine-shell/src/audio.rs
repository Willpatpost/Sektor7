//! Cycle 3 P2 — the event-driven audio capability (owner GO
//! 2026-11-17, §2 P2; Stage 1's audio exclusion is superseded for this
//! module by that authorization).
//!
//! A small, reusable, ONE-SHOT CUE surface over SDL3's callback-free
//! audio-stream model. Design consequences of the charter's
//! performance-first standards:
//!
//! - **Event-driven, never polled:** there is no per-frame API at all.
//!   Cues are pushed as complete prebuilt waveforms through
//!   `SDL_PutAudioStreamData` — a non-blocking buffered copy ("this
//!   call simply copies the unconverted data for later", per the SDL3
//!   documentation) executed by SDL's own audio thread. When idle,
//!   nothing in the engine calls into audio: the bound streams simply
//!   run dry.
//! - **No callback, no runtime synthesis, no per-event allocation:**
//!   the cue waveforms are procedurally generated ONCE at
//!   initialization (pure functions of the sample index — original
//!   placeholder audio, no external assets, no licensing surface);
//!   each playback is one SDL call plus SDL's internal copy.
//! - **One stream per cue, mixed by SDL:** each cue owns a stream
//!   bound to the shared device; SDL mixes concurrently-bound streams,
//!   so distinct cues overlap naturally.
//! - **Audio is a presentation consumer, never an authority:** the
//!   deterministic headless paths never construct this type at all;
//!   interactive paths route gameplay events through [`Audio::play`]
//!   only. No simulation outcome can depend on audio.
//! - **Graceful, observable degradation:** a missing audio device (or
//!   any open failure) degrades to the [`AudioStatus::Silent`] state
//!   with the reason queryable through [`Audio::status`] — never an
//!   error; a mid-run playback failure degrades the same way. The
//!   explicit [`Audio::disabled`] headless mode never touches SDL
//!   audio at all.

use crate::platform::Platform;
use sdl3::audio::{AudioDevice, AudioFormat, AudioSpec, AudioStreamOwner};

/// The gameplay cue surface — the narrow event→sound mapping the
/// application layer wires (firing, impact, damage, interaction,
/// encounter completion). The placeholder synthesis behind each cue
/// is original and procedural; final soundtrack, voice, and spatial
/// audio are out of scope for this milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cue {
    /// A weapon firing.
    Fire,
    /// A projectile or body impact.
    Impact,
    /// Damage applied to a combatant.
    Damage,
    /// An interaction confirmed (switch, pickup).
    Interact,
    /// An encounter completed.
    EncounterComplete,
}

/// Every cue, in the stable index order used internally.
pub const CUES: [Cue; 5] = [
    Cue::Fire,
    Cue::Impact,
    Cue::Damage,
    Cue::Interact,
    Cue::EncounterComplete,
];

impl Cue {
    fn index(self) -> usize {
        match self {
            Cue::Fire => 0,
            Cue::Impact => 1,
            Cue::Damage => 2,
            Cue::Interact => 3,
            Cue::EncounterComplete => 4,
        }
    }
}

/// The observable audio state — the run summary prints this so an
/// unavailable device is VISIBLE, not silent-by-accident.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioStatus {
    /// A playback device is open; cues play through it.
    Active { driver: String, device: String },
    /// No audio: the reason is always carried (headless mode, no
    /// device, an open failure, or a mid-run playback failure).
    Silent { reason: String },
}

impl AudioStatus {
    /// True when cues play.
    pub fn is_active(&self) -> bool {
        matches!(self, AudioStatus::Active { .. })
    }
}

/// The audio capability: a default playback device plus one bound
/// stream per cue, carrying prebuilt procedural waveforms. Drop
/// follows Rust's declaration-order field drops: the device closes
/// first — `SDL_CloseAudioDevice` unbinds the still-bound streams
/// without destroying them (SDL 3.4.18, `DestroyLogicalAudioDevice`)
/// — then each stream destroys itself, and the refcounted SDL audio
/// subsystem quits only after the last stream's refcount clone drops
/// (every stream owner holds one). The reverse order is equally
/// SDL-safe: the mid-run failure path in `play` releases the streams
/// before the device.
pub struct Audio {
    status: AudioStatus,
    /// The open device — Rust drops fields in declaration order, so
    /// this closes FIRST; `SDL_CloseAudioDevice` unbinds the
    /// still-bound streams without destroying them, so the later
    /// stream drops operate on already-unbound streams.
    device: Option<AudioDevice>,
    /// One stream per cue, bound to the device (SDL mixes them).
    streams: Option<Vec<AudioStreamOwner>>,
    /// The prebuilt cue waveforms (48 kHz mono f32), indexed by cue.
    samples: Vec<Vec<f32>>,
}

/// The cue sample rate (Hz).
const SAMPLE_RATE: i32 = 48_000;

impl Audio {
    /// The explicit silent/headless mode: no SDL audio is touched at
    /// all (no subsystem init, no device, no threads), `play` is a
    /// no-op, and the carried reason is observable in the status.
    pub fn disabled(reason: &str) -> Audio {
        Audio {
            status: AudioStatus::Silent {
                reason: reason.to_string(),
            },
            device: None,
            streams: None,
            samples: Vec::new(),
        }
    }

    /// Initializes the default playback device and the per-cue
    /// streams, synthesizing the cue waveforms once. ANY failure —
    /// the audio subsystem, the device open, the stream creation or
    /// binding — degrades to the observable silent state with the
    /// reason: audio is never a gameplay prerequisite.
    pub fn initialize(platform: &Platform) -> Audio {
        let subsystem = match platform.sdl().audio() {
            Ok(subsystem) => subsystem,
            Err(error) => {
                return Audio::disabled(&format!("the audio subsystem: {error}"));
            }
        };
        // The device spec is a REQUEST; SDL3 converts every bound
        // stream into whatever the device actually provides.
        let device_spec = AudioSpec {
            freq: Some(SAMPLE_RATE),
            channels: Some(2),
            format: Some(AudioFormat::F32LE),
        };
        let device = match subsystem.open_playback_device(&device_spec) {
            Ok(device) => device,
            Err(error) => {
                return Audio::disabled(&format!("no audio playback device: {error}"));
            }
        };
        // The application-side cue format: mono f32 at the synthesis
        // rate; SDL's stream converts (and upmixes) to the device.
        let cue_spec = AudioSpec {
            freq: Some(SAMPLE_RATE),
            channels: Some(1),
            format: Some(AudioFormat::F32LE),
        };
        let mut streams = Vec::with_capacity(CUES.len());
        for _ in CUES {
            match subsystem.new_playback_stream(&cue_spec, None) {
                Ok(stream) => streams.push(stream),
                Err(error) => {
                    return Audio::disabled(&format!("the cue stream: {error}"));
                }
            }
        }
        let bound: Vec<&sdl3::audio::AudioStream> =
            streams.iter().map(|stream| &**stream).collect();
        if let Err(error) = device.bind_streams(&bound) {
            return Audio::disabled(&format!("binding the cue streams: {error}"));
        }
        let samples: Vec<Vec<f32>> = CUES.iter().map(|cue| cue_samples(*cue)).collect();
        let driver = subsystem.current_audio_driver().to_string();
        let device_name = device
            .name()
            .map(|name| name.to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        Audio {
            status: AudioStatus::Active {
                driver,
                device: device_name,
            },
            device: Some(device),
            streams: Some(streams),
            samples,
        }
    }

    /// The observable status (print it in the run summary).
    pub fn status(&self) -> &AudioStatus {
        &self.status
    }

    /// Plays one cue: a single non-blocking push of the cue's
    /// prebuilt waveform into its stream (SDL's audio thread consumes
    /// it; no allocation, no synthesis, no polling). A no-op in the
    /// silent state; a mid-run playback failure degrades to the
    /// observable silent state and releases the audio resources.
    pub fn play(&mut self, cue: Cue) {
        let Some(streams) = self.streams.as_mut() else {
            return;
        };
        let index = cue.index();
        if let Err(error) = streams[index].put_data_f32(&self.samples[index]) {
            self.status = AudioStatus::Silent {
                reason: format!("playback failed mid-run: {error}"),
            };
            self.streams = None;
            self.device = None;
            self.samples.clear();
        }
    }
}

/// Deterministic per-index pseudo-noise (a pure splitmix-style integer
/// hash — no RNG state, no wall-clock, byte-identical on every
/// platform and every run).
fn deterministic_noise(index: u32) -> f32 {
    let mut z = index.wrapping_mul(0x9E37_79B1).wrapping_add(0x632B_E597);
    z = (z ^ (z >> 16)).wrapping_mul(0x85EB_CA6B);
    z = (z ^ (z >> 13)).wrapping_mul(0xC2B2_AE35);
    z ^= z >> 16;
    (z >> 8) as f32 / 16_777_216.0 - 0.5
}

/// One pure-function sample generator per cue (t = seconds since the
/// cue start). Original placeholder synthesis: envelopes over sine
/// tones and the deterministic noise — no external assets.
fn fire_sample(t: f32, index: u32) -> f32 {
    // A short sharp noise burst with a fast exponential decay.
    let envelope = (-t * 45.0).exp();
    deterministic_noise(index) * envelope * 0.7
}

fn impact_sample(t: f32, _index: u32) -> f32 {
    // A descending pitch thump: 170 Hz falling to 70 Hz over the cue.
    let sweep = 170.0 - 90.0 * (t / 0.13);
    let tone = (std::f32::consts::TAU * sweep * t).sin();
    let envelope = (-t * 20.0).exp();
    tone * envelope * 0.8
}

fn damage_sample(t: f32, index: u32) -> f32 {
    // A harsh mid-range burst: a 210 Hz near-square with noise and a
    // fast attack, medium decay.
    let square = if (t * 210.0).fract() < 0.5 { 1.0 } else { -1.0 };
    let envelope = (-t * 14.0).exp() * (t * 300.0).min(1.0);
    (square * 0.5 + deterministic_noise(index) * 0.3) * envelope * 0.6
}

fn interact_sample(t: f32, _index: u32) -> f32 {
    // A soft two-tone confirmation blip: 660 Hz then 880 Hz.
    let (freq, local) = if t < 0.055 {
        (660.0, t)
    } else {
        (880.0, t - 0.055)
    };
    let envelope = (-local * 30.0).exp();
    (std::f32::consts::TAU * freq * t).sin() * envelope * 0.45
}

fn complete_sample(t: f32, _index: u32) -> f32 {
    // A rising three-note arpeggio: C5, E5, G5, each ~150 ms.
    let notes = [523.25, 659.25, 783.99];
    let note = (t / 0.15).floor() as usize;
    let local = t - note as f32 * 0.15;
    let freq = notes[note.min(2)];
    let envelope = (-local * 12.0).exp() * (local * 200.0).min(1.0);
    (std::f32::consts::TAU * freq * t).sin() * envelope * 0.5
}

/// Synthesizes one cue's waveform — a pure function of the cue,
/// deterministic on every run. Cross-platform float bit-identity is not
/// asserted (libm `sin`/`exp` are not guaranteed bit-portable); audio sits
/// outside every determinism pin, so only per-run determinism is claimed.
fn cue_samples(cue: Cue) -> Vec<f32> {
    let (duration_s, generator): (f32, fn(f32, u32) -> f32) = match cue {
        Cue::Fire => (0.09, fire_sample),
        Cue::Impact => (0.13, impact_sample),
        Cue::Damage => (0.18, damage_sample),
        Cue::Interact => (0.11, interact_sample),
        Cue::EncounterComplete => (0.45, complete_sample),
    };
    let count = (duration_s * SAMPLE_RATE as f32) as usize;
    (0..count)
        .map(|index| generator(index as f32 / SAMPLE_RATE as f32, index as u32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The explicit silent mode: no SDL audio is touched, play is a
    /// no-op, and the observable cost of a refused cue is effectively
    /// zero (the no-op branch — no SDL call at all).
    #[test]
    fn disabled_audio_is_silent_and_cheap() {
        let mut audio = Audio::disabled("headless test mode");
        assert_eq!(
            audio.status(),
            &AudioStatus::Silent {
                reason: "headless test mode".to_string()
            }
        );
        assert!(!audio.status().is_active());
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            audio.play(Cue::Fire);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "100k refused cues stay cheap: {elapsed:?}"
        );
    }

    /// The cue waveforms are pure: identical bytes on regeneration,
    /// every sample finite, every length bounded, and every cue
    /// carries actual energy (never digital silence).
    #[test]
    fn cue_waveforms_are_deterministic_finite_bounded_and_audible() {
        for cue in CUES {
            let first = cue_samples(cue);
            let second = cue_samples(cue);
            assert_eq!(first, second, "{cue:?} regenerates identically");
            assert!(
                first.len() < SAMPLE_RATE as usize,
                "{cue:?} is bounded under one second ({} samples)",
                first.len()
            );
            assert!(
                first.iter().all(|s| s.is_finite()),
                "{cue:?} has no non-finite samples"
            );
            let energy: f32 = first.iter().map(|s| s * s).sum();
            assert!(
                energy > 1.0,
                "{cue:?} carries energy (sum of squares {energy})"
            );
        }
    }
}
