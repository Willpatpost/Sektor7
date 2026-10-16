# SEKTOR 7

A cyberpunk FPS-RPG set in one neon night-district — fixer jobs, ripperdoc
chrome, hovercars, and mercs who don't ask questions.

This repo holds two lines of the same game:

- **`browser/`** — the original Three.js + Vite vertical slice. Complete,
  playable single-file build (`npm run build` → one HTML file), everything
  procedural: city, textures, audio. Serves as the design reference.
  See `browser/README.md`.
- **`godot/`** — the Godot 4 main line, ported system-by-system from the
  browser prototype toward a full-fidelity engine build. See `godot/README.md`
  for current milestone status and how to run it.

## Quick start

```bash
# browser prototype (instant, zero install beyond deps)
cd browser && npm install && npm run dev

# godot main line (needs the Godot 4.7 editor, or headless test)
cd godot && godot4 --headless --path . -s res://tests/smoke.gd
```

## Gameplay

WASD move · mouse look · LMB fire · R reload · Shift sprint · Space jump /
fly up · C fly down · E interact / hovercar enter-exit · V camera POV ·
Esc pause. Xbox controller supported on both lines.
