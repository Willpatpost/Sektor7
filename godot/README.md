# SEKTOR 7 — Godot port

Godot 4 port of the Sektor7 browser cyberpunk FPS-RPG (`../browser/`),
started 2026-10-16 as the long-term engine line. The browser build remains
the design reference; this project ports its systems one milestone at a
time, verified headless from the CLI.

## Status — Milestone 2 (gameplay parity core)

Milestone 1 — systems skeleton:
- Procedural city (grid blocks, emissive window facades, neon Label3D
  signs, perimeter wall, street lamps with real OmniLights, spawn arch,
  ripperdoc terminal), StaticBody3D physics collision
- Environment: ProceduralSky night palette, fog + volumetric fog, glow,
  shadowed moonlight (Forward Plus renderer)
- FPS player (CharacterBody3D, mouse capture look, sprint/jump, rain)

Milestone 2 — quests, economy, cameras, audio:
- 4-job VEX quest chain (plaza mercs → data shard → drones → RAZORTAIL)
  with typewriter dialogue, objective HUD, quest marker beam, shard
  pickup, rewards, win state, free-roam resume
- Ripperdoc shop at the terminal: 4 upgrade lines × 3 tiers, ammo
  refill, eddies economy
- Enemy roster: mercs, orbiting laser drones, RAZORTAIL boss with
  taunt dialogue, eddies/ammo/stim pickups with Area3D collection
- Five camera POVs cycled with V — first person, third behind
  close/far, front close/far — with raycast wall pull-in and a visible
  player body in third person; weapon aim and hovercar thrust keyed to
  the head basis so front views never invert control
- WASP SMG with auto-fire, unlock after job 1, weapon switching
- Procedural audio: SFX synthesized into AudioStreamWAV at runtime
  (shoot, hit, hurt, pickup, level, quest, boom, ui) plus a
  pitch-shifting hover engine loop
- Four hovercars with full 3D flight (floating-mode physics, slide
  bounces, rooftop landings, E enter/exit)

Not yet ported (next milestones): web export (restores a playable
browser link), weapon viewmodels/tracers/FX polish, music, real assets
toward the Skyrim-tier fidelity goal, jetpacks.

## Run

Open this folder in the Godot 4.7 editor (godotengine.org), or headless
test from the CLI (needs `godot4` on PATH):

```bash
godot4 --headless --path . --import
godot4 --headless --path . -s res://tests/smoke.gd
```

Controls: WASD move · mouse look · LMB fire · R reload · 1/2 weapons ·
Shift sprint · Space jump / fly up · C fly down · E interact / dialogue /
hovercar enter-exit · V camera POV · Esc pause. Gamepad: sticks, RT fire,
B interact, RB/LB fly, X reload, Y weapon cycle.
