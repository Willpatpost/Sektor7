//! Render-only encounter models and transients. The same immutable local mesh
//! supplies both cameras. Resolved volleys own the world-space traces until
//! their presentation lifetimes expire, independently of the simulation clock.

mod model;
#[cfg(test)]
mod tests;

use std::time::Duration;

use engine_core::weapons::MAX_SPREAD_RAYS;
use engine_core::{CameraMode, CameraPose, WeaponId, look_direction};
use engine_renderer::scene::SceneVertex;

use crate::encounter_demo::EncounterDemo;
use crate::encounter_events::{PresentationEvent, RayFact, RayImpact};

type Point = [f32; 3];

const VOLLEY_CAPACITY: usize = 8;
const IMPACT_TIME: Duration = Duration::from_millis(220);
const RECOIL_TIME: Duration = Duration::from_millis(125);
const FLASH_TIME: Duration = Duration::from_millis(65);

fn trace_time(weapon: WeaponId) -> Duration {
    Duration::from_millis(match weapon {
        WeaponId::Precision => 125,
        WeaponId::Spread => 100,
    })
}

fn add(a: Point, b: Point) -> Point {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn mul(v: Point, k: f32) -> Point {
    [v[0] * k, v[1] * k, v[2] * k]
}

fn dot(a: Point, b: Point) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Point, b: Point) -> Point {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn unit(v: Point) -> Option<Point> {
    let length = dot(v, v).sqrt();
    (length.is_finite() && length > 1e-6).then(|| mul(v, length.recip()))
}

#[derive(Clone, Copy)]
struct Frame {
    origin: Point,
    right: Point,
    up: Point,
    back: Point,
}

impl Frame {
    fn looking(origin: Point, forward: Point) -> Self {
        let back = mul(forward, -1.0);
        // Player pitch is clamped away from both poles; an exactly vertical
        // synthetic ray still gets a well-defined frame for an impact.
        let right = unit([back[2], 0., -back[0]]).unwrap_or([1., 0., 0.]);
        Self {
            origin,
            right,
            up: cross(back, right),
            back,
        }
    }

    fn vector(&self, [x, y, z]: Point) -> Point {
        add(add(mul(self.right, x), mul(self.up, y)), mul(self.back, z))
    }

    fn point(&self, p: Point) -> Point {
        add(self.origin, self.vector(p))
    }

    fn compose(&self, local: Self) -> Self {
        Self {
            origin: self.point(local.origin),
            right: self.vector(local.right),
            up: self.vector(local.up),
            back: self.vector(local.back),
        }
    }

    fn append(&self, out: &mut Vec<SceneVertex>, mesh: &[SceneVertex]) {
        out.extend(mesh.iter().map(|v| SceneVertex {
            pos: self.point(v.pos),
            col: v.col,
        }));
    }
}

fn remaining(age: Duration, lifetime: Duration) -> f32 {
    1. - (age.as_secs_f32() / lifetime.as_secs_f32()).min(1.)
}

// Local to eye space. The soft inward cant brings the bore toward the reticle.
// Preserve the vertical FOV and narrow the horizontal offset on narrow windows.
fn first_person_mount(aspect: f32, recoil: f32) -> Frame {
    let aspect = if aspect.is_finite() && aspect > 0. {
        aspect
    } else {
        16. / 9.
    };
    let x = (aspect * 0.14).clamp(0., 0.22);
    Frame::looking(
        [x, -0.205, -0.53 + 0.060 * recoil],
        look_direction(x * 0.55, 0.10 + 0.075 * recoil),
    )
}

// The shoulder stays at standing height while the bore follows aim in 3-D.
fn third_person_mount(eye: Frame, recoil: f32) -> Frame {
    let vertical_offset = [0., -0.31, 0.];
    let origin = add(
        add(
            eye.point([0.405, 0., -0.23 + 0.06 * recoil]),
            vertical_offset,
        ),
        mul(eye.up, 0.015 * recoil),
    );
    let mut mount = eye.compose(Frame::looking(
        [0.; 3],
        look_direction(0.037, 0.045 * recoil),
    ));
    mount.origin = origin;
    mount
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VisualStats {
    pub live_volleys: usize,
    pub live_rays: usize,
    pub expired_volleys: u64,
    pub evicted_volleys: u64,
    pub stale_volleys: u64,
    /// Invalid input is refused as a whole volley, never partly drawn.
    pub invalid_volleys: u64,
}

struct LiveVolley {
    id: u32,
    weapon: WeaponId,
    eye: Frame,
    rays: Vec<RayFact>,
    age: Duration,
    lifetime: Duration,
}

impl Default for LiveVolley {
    fn default() -> Self {
        Self {
            id: 0,
            weapon: WeaponId::Precision,
            eye: Frame::looking([0.; 3], [0., 0., -1.]),
            rays: Vec::with_capacity(MAX_SPREAD_RAYS as usize),
            age: Duration::ZERO,
            lifetime: Duration::ZERO,
        }
    }
}

pub struct EncounterVisuals {
    precision: Vec<SceneVertex>,
    spread: Vec<SceneVertex>,
    operator: model::OperatorMesh,
    slots: [LiveVolley; VOLLEY_CAPACITY],
    firing: Option<(WeaponId, Duration)>,
    stats: VisualStats,
}

impl Default for EncounterVisuals {
    fn default() -> Self {
        Self {
            precision: model::build(WeaponId::Precision),
            spread: model::build(WeaponId::Spread),
            operator: model::OperatorMesh::new(),
            slots: std::array::from_fn(|_| LiveVolley::default()),
            firing: None,
            stats: VisualStats::default(),
        }
    }
}

impl EncounterVisuals {
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            slot.rays.clear();
        }
        self.stats = VisualStats::default();
        self.firing = None;
    }

    pub fn stats(&self) -> VisualStats {
        self.stats
    }

    pub fn advance(&mut self, elapsed: Duration) {
        if let Some((_, age)) = &mut self.firing {
            *age = age.saturating_add(elapsed);
            if *age >= RECOIL_TIME {
                self.firing = None;
            }
        }
        if self.stats.live_volleys == 0 {
            return;
        }
        for slot in &mut self.slots {
            if slot.rays.is_empty() {
                continue;
            }
            slot.age = slot.age.saturating_add(elapsed);
            if slot.age >= slot.lifetime {
                self.stats.expired_volleys += 1;
                self.stats.live_volleys -= 1;
                self.stats.live_rays -= slot.rays.len();
                slot.rays.clear();
            }
        }
    }

    /// Age is the time already elapsed between the shot's logical tick and
    /// presentation of this frame (including catch-up in a multi-tick frame).
    pub fn observe(&mut self, event: &PresentationEvent, age: Duration) {
        let volley = match event {
            PresentationEvent::Fired(volley) => volley,
            PresentationEvent::RoundStarted => return self.clear(),
            PresentationEvent::Switched(_) => {
                self.firing = None;
                return;
            }
            _ => return,
        };
        let finite = |p: Point| p.into_iter().all(f32::is_finite);
        let Some(aim) = unit(volley.aim) else {
            self.stats.invalid_volleys += 1;
            return;
        };
        if !finite(volley.origin)
            || volley.rays.is_empty()
            || volley.rays.len() > MAX_SPREAD_RAYS as usize
            || volley.rays.iter().any(|r| {
                !finite(r.endpoint)
                    || unit(r.direction).is_none()
                    || !dot(
                        sub(r.endpoint, volley.origin),
                        sub(r.endpoint, volley.origin),
                    )
                    .is_finite()
            })
        {
            self.stats.invalid_volleys += 1;
            return;
        }
        let lifetime = if volley.rays.iter().any(|r| is_impact(r.impact)) {
            IMPACT_TIME
        } else {
            trace_time(volley.weapon)
        };
        if age >= lifetime {
            self.stats.stale_volleys += 1;
            return;
        }
        // Never split a volley. Under overload reuse the oldest slot; ties are
        // broken by the earlier accepted firing ID. In normal play one slot
        // per weapon already suffices at the encounter's two cooldowns.
        let index = self
            .slots
            .iter()
            .position(|s| s.rays.is_empty())
            .unwrap_or_else(|| {
                self.slots
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, s)| (s.age, std::cmp::Reverse(s.id)))
                    .unwrap()
                    .0
            });
        let slot = &mut self.slots[index];
        if slot.rays.is_empty() {
            self.stats.live_volleys += 1;
        } else {
            self.stats.evicted_volleys += 1;
            self.stats.live_rays -= slot.rays.len();
        }
        slot.rays.clear();
        slot.rays.extend_from_slice(&volley.rays);
        slot.id = volley.id;
        slot.age = age;
        slot.lifetime = lifetime;
        slot.weapon = volley.weapon;
        slot.eye = Frame::looking(volley.origin, aim);
        self.stats.live_rays += slot.rays.len();
        if age < RECOIL_TIME {
            self.firing = Some((volley.weapon, age));
        }
    }

    /// Append to caller-owned scratch. Only first-person geometry uses camera
    /// space; the caller supplies projection alone to its foreground draw.
    pub fn append(
        &self,
        world: &mut Vec<SceneVertex>,
        foreground: &mut Vec<SceneVertex>,
        demo: &EncounterDemo,
        pose: CameraPose,
        aspect: f32,
    ) {
        let player = demo.driver.state();
        let selected = demo.arsenal.selected();
        let age = self
            .firing
            .filter(|(w, _)| *w == selected)
            .map(|(_, age)| age);
        let kick = age.map_or(0., |age| remaining(age, RECOIL_TIME).powi(2));
        let mesh = match selected {
            WeaponId::Precision => &self.precision,
            WeaponId::Spread => &self.spread,
        };
        let mut eye = player.position;
        eye[1] += demo.driver.config().eye_height;
        let camera_right =
            Frame::looking(pose.position, look_direction(pose.yaw, pose.pitch)).right;
        let mode = player.camera_mode;
        {
            let (dest, mount) = match mode {
                CameraMode::FirstPerson => (&mut *foreground, first_person_mount(aspect, kick)),
                CameraMode::ThirdPerson => {
                    let upright = Frame::looking(eye, look_direction(player.yaw, 0.));
                    let mount = third_person_mount(
                        Frame::looking(eye, look_direction(player.yaw, player.pitch)),
                        kick,
                    );
                    self.operator.append(world, upright, mount);
                    (&mut *world, mount)
                }
            };
            mount.append(dest, mesh);
            if let Some(age) = age.filter(|a| *a < FLASH_TIME) {
                muzzle_flash(dest, &mount, selected, remaining(age, FLASH_TIME));
            }
        }
        if self.stats.live_rays > 0 {
            for slot in self.slots.iter().filter(|s| !s.rays.is_empty()) {
                append_volley(world, slot, pose.position, camera_right, mode, aspect);
            }
        }
    }
}

fn is_impact(impact: RayImpact) -> bool {
    !matches!(impact, RayImpact::Miss | RayImpact::OutOfRange)
}

fn two_sided(out: &mut Vec<SceneVertex>, a: Point, b: Point, c: Point, color: Point) {
    out.extend([a, b, c, c, b, a].map(|pos| SceneVertex { pos, col: color }));
}

fn muzzle_flash(out: &mut Vec<SceneVertex>, mount: &Frame, weapon: WeaponId, glow: f32) {
    let center = mount.point(model::tip(weapon));
    let (width, length, color) = match weapon {
        WeaponId::Precision => (0.065, 0.24, [0.50, 0.98, 1.]),
        WeaponId::Spread => (0.14, 0.16, [1., 0.83, 0.31]),
    };
    let radius = width * (0.35 + 0.65 * glow);
    let tip = add(center, mul(mount.back, -length * glow));
    let rear = add(center, mul(mount.back, 0.02));
    let points = [
        mount.up,
        mount.right,
        mul(mount.up, -1.),
        mul(mount.right, -1.),
    ]
    .map(|axis| add(add(center, mul(mount.back, -0.025)), mul(axis, radius)));
    for i in 0..4 {
        let j = (i + 1) % 4;
        two_sided(out, points[i], points[j], tip, color);
        two_sided(out, points[j], points[i], rear, [1., 0.98, 0.86]);
    }
}

fn append_volley(
    out: &mut Vec<SceneVertex>,
    slot: &LiveVolley,
    camera: Point,
    camera_right: Point,
    mode: CameraMode,
    aspect: f32,
) {
    let age = slot.age;
    let beam = remaining(age, trace_time(slot.weapon));
    // A resolved flash of light stays attached to the pose it was emitted
    // from while the currently equipped weapon is free to recover from recoil.
    let mount = match mode {
        CameraMode::FirstPerson => slot.eye.compose(first_person_mount(aspect, 1.)),
        CameraMode::ThirdPerson => third_person_mount(slot.eye, 1.),
    };
    let muzzle = mount.point(model::tip(slot.weapon));
    let origin = slot.eye.origin;
    let (width, color) = match slot.weapon {
        WeaponId::Precision => (0.021, [0.22, 0.91, 1.0]),
        WeaponId::Spread => (0.016, [1.0, 0.69, 0.19]),
    };
    for ray in &slot.rays {
        if beam > 0. {
            let color = mul(color, 0.50 + 0.50 * beam);
            tracer_strip(
                out,
                [origin, muzzle, ray.endpoint],
                camera,
                camera_right,
                width,
                color,
            );
        }
        if is_impact(ray.impact) {
            impact_burst(out, ray, slot.age);
        }
    }
}

// A single continuous strip from the visual emitter onto the resolved eye
// ray and through its exact endpoint. The two segments share a cross-section;
// a chase camera never sees an extra beam emitted from the character's face.
fn tracer_strip(
    out: &mut Vec<SceneVertex>,
    [eye, muzzle, endpoint]: [Point; 3],
    camera: Point,
    camera_right: Point,
    width: f32,
    color: Point,
) {
    let span = sub(endpoint, eye);
    let length = dot(span, span).sqrt();
    if length < 1e-5 {
        return;
    } // A contact at the eye has only an impact.
    let offset = sub(muzzle, eye);
    let muzzle_distance = dot(offset, offset).sqrt();
    // If the viewmodel intersects close cover, bring the lead-in inside the
    // available distance. It still has visible area from FP and never shoots
    // backward toward the wall from a muzzle already beyond the hit.
    let start = add(eye, mul(offset, (0.45 * length / muzzle_distance).min(1.)));
    let join = add(eye, mul(span, 0.65));
    let side = |from, to| unit(cross(sub(to, from), sub(camera, from))).unwrap_or(camera_right);
    let near_side = side(start, join);
    let mut far_side = side(join, endpoint);
    if dot(near_side, far_side) < 0. {
        far_side = mul(far_side, -1.);
    }
    let join_side = unit(add(near_side, far_side)).unwrap_or(near_side);
    let width = width.min(length * 0.08);
    let edge = |p, side, size| [sub(p, mul(side, size)), add(p, mul(side, size))];
    let edges = [
        edge(start, near_side, width * 0.54),
        edge(join, join_side, width),
        edge(endpoint, far_side, width),
    ];
    for sections in edges.windows(2) {
        let [a, b] = sections[0];
        let [d, c] = sections[1];
        two_sided(out, a, b, c, color);
        two_sided(out, a, c, d, color);
    }
}

fn impact_burst(out: &mut Vec<SceneVertex>, ray: &RayFact, age: Duration) {
    let (radius, color, spokes) = match ray.impact {
        RayImpact::Adversary { damaged: true, .. } => (0.19, [1., 0.19, 0.47], 6),
        RayImpact::Adversary { .. } | RayImpact::Dormant { .. } => (0.13, [0.63, 0.60, 0.85], 4),
        RayImpact::Cover => (0.16, [1., 0.80, 0.33], 4),
        RayImpact::Miss | RayImpact::OutOfRange => return,
    };
    // Face the incoming direction. Bias the spark slightly OUT of the contact
    // to avoid depth fighting, with depth still resolving against intervening
    // world faces. No alternate intersection query or synthetic hit position.
    let incoming = unit(ray.direction).expect("validated ray");
    let frame = Frame::looking(ray.endpoint, incoming);
    let fade = remaining(age, IMPACT_TIME);
    let radius = radius * (1.50 - 0.65 * fade);
    let center = add(ray.endpoint, mul(incoming, -0.014));
    let color = mul(color, 0.45 + 0.55 * fade);
    const SIX: [[f32; 2]; 6] = [
        [1., 0.],
        [0.5, 0.866],
        [-0.5, 0.866],
        [-1., 0.],
        [-0.5, -0.866],
        [0.5, -0.866],
    ];
    const FOUR: [[f32; 2]; 4] = [[0.80, 0.60], [-0.60, 0.80], [-0.80, -0.60], [0.60, -0.80]];
    let directions: &[[f32; 2]] = if spokes == 6 { &SIX } else { &FOUR };
    for [x, y] in directions {
        let radial = add(mul(frame.right, *x), mul(frame.up, *y));
        let side = mul(cross(incoming, radial), radius * 0.18 * fade);
        let point = add(center, mul(radial, radius));
        two_sided(out, sub(center, side), add(center, side), point, color);
    }
}
