//! The encounter's combat HUD. Pixel geometry is rebuilt only when the visible readout changes.
use std::fmt::Write;
use std::time::Duration;

use engine_core::{WeaponId, adversary::AdversaryState};
use engine_renderer::overlay::{OverlayVertex, push_rect_px, push_text_px};

use crate::encounter_demo::{EncounterDemo, EncounterPhase};
use crate::encounter_events::{PresentationEvent, RayImpact};

const PAPER: [f32; 4] = [0.90, 0.96, 0.97, 1.0];
const INK: [f32; 4] = [0.028, 0.043, 0.057, 0.92];
const CYAN: [f32; 4] = [0.23, 0.91, 0.94, 1.0];
const AMBER: [f32; 4] = [1.0, 0.64, 0.19, 1.0];
const HIT: [f32; 4] = [1.0, 0.32, 0.40, 1.0];
const MUTED: [f32; 4] = [0.28, 0.36, 0.41, 1.0];

#[derive(Default)]
pub(crate) struct CombatHud {
    hit: Duration,
    blocked: Duration,
    hurt: Duration,
    switched: Duration,
    last: Option<Readout>,
    targets: Vec<(f32, AdversaryState)>,
    text: String,
    pub vertices: Vec<OverlayVertex>,
}

#[derive(PartialEq)]
struct Readout {
    extent: (u32, u32),
    phase: EncounterPhase,
    health: (f32, f32),
    weapon: WeaponId,
    ready: bool,
    hit: bool,
    blocked: bool,
    hurt: bool,
    switched: bool,
}

impl CombatHud {
    pub fn advance(&mut self, dt: Duration) {
        for timer in [
            &mut self.hit,
            &mut self.blocked,
            &mut self.hurt,
            &mut self.switched,
        ] {
            *timer = timer.saturating_sub(dt);
        }
    }

    pub fn observe(&mut self, event: &PresentationEvent, age: Duration) {
        let remaining = |ms| Duration::from_millis(ms).saturating_sub(age);
        match event {
            PresentationEvent::RoundStarted => self.advance(Duration::MAX),
            PresentationEvent::PlayerDamaged => self.hurt = remaining(470),
            PresentationEvent::Switched(_) => self.switched = remaining(400),
            PresentationEvent::Fired(volley) => {
                self.hit = Duration::ZERO;
                self.blocked = Duration::ZERO;
                for ray in &volley.rays {
                    match ray.impact {
                        RayImpact::Adversary { damaged: true, .. } => self.hit = remaining(230),
                        RayImpact::Cover | RayImpact::Dormant { .. } => {
                            self.blocked = remaining(190)
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    pub fn build(&mut self, demo: &EncounterDemo, extent: (u32, u32)) {
        let player = demo.encounter.player;
        let key = Readout {
            extent,
            phase: demo.encounter.phase,
            health: (player.current, player.maximum),
            weapon: demo.arsenal.selected(),
            ready: demo.weapon_ready(),
            hit: !self.hit.is_zero(),
            blocked: !self.blocked.is_zero(),
            hurt: !self.hurt.is_zero(),
            switched: !self.switched.is_zero(),
        };
        let world = demo.spine.world();
        let targets = (0..world.count()).map(|i| {
            let a = world.adversary(i);
            ((a.health.current / a.health.maximum).clamp(0., 1.), a.state)
        });
        if self.last.as_ref() == Some(&key) && self.targets.iter().copied().eq(targets.clone()) {
            return;
        }
        self.targets.clear();
        self.targets.extend(targets);
        self.vertices.clear();
        let (w, h) = (extent.0 as f32, extent.1 as f32);
        if w < 1. || h < 1. {
            self.last = None;
            return;
        }
        let scale = (h / 480.).clamp(0.75, 2.).min(w / 275.);
        let mut canvas = Canvas {
            vertices: &mut self.vertices,
            extent,
            scale,
        };
        let w = w / scale;
        let h = h / scale;
        let cx = w * 0.5;
        let cy = h * 0.5;
        let accent = match key.weapon {
            WeaponId::Precision => CYAN,
            WeaponId::Spread => AMBER,
        };
        if !key.phase.is_terminal() {
            let gap = if key.weapon == WeaponId::Spread {
                8.
            } else {
                4.
            };
            for (x, y, dx, dy) in [
                (-gap - 4., -0.5, 4., 1.),
                (gap, -0.5, 4., 1.),
                (-0.5, -gap - 4., 1., 4.),
                (-0.5, gap, 1., 4.),
            ] {
                canvas.rect(cx + x - 0.7, cy + y - 0.7, dx + 1.4, dy + 1.4, INK);
                canvas.rect(
                    cx + x,
                    cy + y,
                    dx,
                    dy,
                    if key.ready { PAPER } else { AMBER },
                );
            }
            if key.hit {
                for sx in [-1., 1.] {
                    for sy in [-1., 1.] {
                        for i in 0..4 {
                            let d = 5. + i as f32;
                            canvas.rect(cx + sx * d - 0.8, cy + sy * d - 0.8, 1.6, 1.6, HIT);
                        }
                    }
                }
            }
            if key.blocked {
                canvas.rect(cx - 26., cy - 27., 52., 12., INK);
                canvas.text(cx - 22., cy - 25., "COVER", AMBER);
            }
        }
        canvas.rect(8., h - 48., 103., 40., INK);
        canvas.rect(8., h - 48., 2., 40., if key.hurt { HIT } else { CYAN });
        self.text.clear();
        let _ = write!(self.text, "HP {:.0}/{:.0}", key.health.0, key.health.1);
        canvas.text(15., h - 44., &self.text, if key.hurt { HIT } else { PAPER });
        canvas.rect(15., h - 34., 89., 3., MUTED);
        canvas.rect(
            15.,
            h - 34.,
            89. * (key.health.0 / key.health.1).clamp(0., 1.),
            3.,
            if key.hurt { HIT } else { CYAN },
        );
        // One segment per roster slot in spawn order. Dormant and defeated units
        // retain their slots so activation and defeat do not move the others.
        canvas.text(15., h - 28., "HOSTILES", PAPER);
        let slot = 89. / self.targets.len().max(1) as f32;
        for (i, (hp, state)) in self.targets.iter().enumerate() {
            let x = 15. + i as f32 * slot;
            let width = (slot - 2.).max(0.);
            canvas.rect(x, h - 16., width, 4., MUTED);
            canvas.rect(
                x,
                h - 16.,
                width * hp,
                4.,
                match state {
                    AdversaryState::Dormant => [0.59, 0.64, 0.70, 1.],
                    _ => HIT,
                },
            );
        }
        canvas.rect(w - 122., h - 39., 114., 31., INK);
        canvas.rect(w - 122., h - 39., 2., 31., accent);
        let name = match key.weapon {
            WeaponId::Precision => "01 PRECISION",
            WeaponId::Spread => "02 SPREAD",
        };
        canvas.text(w - 115., h - 34., name, accent);
        let status = if key.phase.is_terminal() {
            "ROUND OVER"
        } else if key.phase == EncounterPhase::Dormant {
            "STANDBY"
        } else if key.switched && key.ready {
            "EQUIPPED"
        } else if key.ready {
            "READY"
        } else {
            "COOLING"
        };
        canvas.text(w - 115., h - 19., status, PAPER);
        if key.hurt && !key.phase.is_terminal() {
            canvas.rect(cx - 31., cy - 46., 62., 13., INK);
            canvas.text(cx - 26., cy - 43., "DAMAGE", HIT);
        }
        let message = match key.phase {
            EncounterPhase::Dormant => Some(("CROSS THE LINE", PAPER)),
            EncounterPhase::Won { .. } => Some(("VICTORY  [R] RESET", CYAN)),
            EncounterPhase::Lost { .. } => Some(("DEFEAT  [R] RESET", HIT)),
            EncounterPhase::Active => None,
        };
        if let Some((label, color)) = message {
            let width = label.len() as f32 * 9. + 12.;
            canvas.rect(cx - width * 0.5, h * 0.72, width, 18., INK);
            canvas.text(cx - width * 0.5 + 6., h * 0.72 + 5., label, color);
        }
        self.last = Some(key);
    }
}

struct Canvas<'a> {
    vertices: &'a mut Vec<OverlayVertex>,
    extent: (u32, u32),
    scale: f32,
}
impl Canvas<'_> {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        let s = self.scale;
        push_rect_px(
            self.vertices,
            self.extent,
            [x * s, y * s, w * s, h * s],
            color,
        );
    }
    fn text(&mut self, x: f32, y: f32, text: &str, color: [f32; 4]) {
        let s = self.scale;
        push_text_px(self.vertices, self.extent, [x * s, y * s], s, text, color);
    }
}
