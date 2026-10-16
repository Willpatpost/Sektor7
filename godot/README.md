# SEKTOR 7 — Godot port

Godot 4 port of the Sektor7 browser cyberpunk FPS-RPG (`~/Sektor7`), started
2026-10-16 as the long-term engine line after the user chose an engine port
over pushing Three.js further. The browser build remains the reference
prototype; this project ports its design system by system.

## Status — Milestone 1 (systems skeleton)

Ported so far:
- Procedural city (grid blocks, emissive window facades, neon Label3D signs,
  perimeter wall, street lamps with real OmniLights, spawn arch), physics
  collision via StaticBody3D boxes
- Environment: ProceduralSky night palette, fog + volumetric fog, glow,
  shadowed moonlight (Forward Plus renderer)
- FPS player (CharacterBody3D, mouse capture look, sprint/jump, HP/XP/eddies,
  rain particles)
- Hitscan weapon with reload, physics-ray targeting
- Merc enemy AI (patrol / combat / burst fire with LOS raycasts)
- Four hovercars with full 3D flight (camera-relative thrust, Space/C or
  RB/LB vertical, floating-mode collision, rooftop landings, E enter/exit)
- Minimal HUD, menu/pause states, gamepad mappings

Not yet ported (next milestones): quests/VEX chain, ripperdoc shop, POVs,
pickups, boss, drones, audio, weapons viewmodels, graphics polish passes,
web export.

## Run

Open this folder in the Godot 4.7 editor (download from godotengine.org), or
headless test from this machine:

```bash
godot4 --headless --path . --import
godot4 --headless --path . -s res://tests/smoke.gd
```

Controls: WASD move · mouse look · LMB fire · R reload · Shift sprint ·
Space jump / fly up · C fly down · E interact / hovercar enter-exit ·
Esc pause. Gamepad: sticks + RT fire + B interact + RB/LB fly.
