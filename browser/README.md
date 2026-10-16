# SEKTOR 7

A browser cyberpunk FPS-RPG vertical slice — one night district, a fixer's job
chain, gunplay, drones, chrome. Built with Three.js; every asset (city, signs,
sounds, music) is generated procedurally at runtime. No asset files.

## Run

```bash
npm install
npm run dev        # dev server (prints a localhost URL)
```

Or play the prebuilt single file: open `dist/index.html` directly in a browser
(after `npm run build`).

## Commands

| command | what it does |
|---|---|
| `npm run dev` | vite dev server with hot reload |
| `npm run build` | production build → single self-contained `dist/index.html` |
| `npm run preview` | serve the production build |
| `npm run smoke` | headless node test: world layout, movement, AI, weapons, full quest chain |

## Controls

Keyboard + mouse: WASD move · mouse look · LMB fire · R reload · 1/2 weapons ·
wheel cycle · Shift sprint · Space jump · E/Enter interact / advance calls ·
Esc pause. Hovercar flight: E enters/exits, W/S thrust toward where you look,
A/D strafe, Space/C up/down, LMB drive-by — you can land on rooftops. **V**
cycles five camera POVs: first person, third person behind close/far, front
close/far (crosshair hidden in front views; aim always follows your look
direction)

Xbox controller (Gamepad API — works even where mouse pointer-lock is
blocked, e.g. embedded previews): left stick analog move · right stick look ·
RT fire · X reload · Y weapon · A jump/confirm · B interact/back · RB/LB fly
up/down · L3 sprint ·
Start pause · stick/d-pad + A navigate the ripperdoc shop. E/Enter acts as
confirm on every screen; controller A does the same on menus.

If the browser blocks pointer lock (embedded previews), the game auto-enables
edge-steer mode: move the cursor toward the screen edges to turn, keep it
centered to aim, click to fire. Opening the page in its own tab restores
standard mouselock.

## The slice

- **Open city** — procedurally generated 8×8 block district: towers with lit
  windows, neon signage, roof trim, holo-ad pylons, parked cars, flying
  vehicles, pedestrians, rain, fog, bloom.
- **FPS combat** — pistol + unlockable SMG, hitscan with tracers and spread,
  recoil, reload; gang mercs (patrol/alert/combat AI) and recon drones; a
  mini-boss, RAZORTAIL.
- **Story & RPG** — VEX the fixer runs a 4-job chain (clear the plaza, recover
  a data shard, down the drones, kill the captain) with holo dialogue, quest
  markers, XP/levels, eddies, and a ripperdoc shop with upgradeable cyberware.
- **Vehicles** — four named hovercars parked around the district (E to
  enter/exit): full 3D flight — thrust toward where you look, strafe, climb
  and descend (~94 km/h max, ceiling 95m), idle hover close to the ground,
  rooftop landings, crash bounces with sparks, low-altitude roadkill damage,
  drive-by shooting, thruster pods with speed-reactive glow and synthesized
  engine audio, full gamepad support. Jetpacks for the player and NPCs are
  the planned next step on this flight code.
- **Graphics pass** — procedural neon environment map (PMREM) driving
  wet-street and vehicle reflections with puddle roughness-mapped asphalt,
  moon-and-cloud sky with horizon pollution glow, grime-streaked facades with
  storefront glow bands, hanging neon signs and animated billboards,
  volumetric lamp cones, contact shadows, heavier gusting rain, MSAA, and a
  cinematic grade pass (vignette, speed-reactive chromatic aberration, film
  grain).

## Structure

```
index.html      HUD overlay + styling (DOM HUD, menus, shop, dialogue)
src/main.js     bootstrap, state machine, input, game loop, postprocessing
src/world.js    city layout (pure data) + visuals (procedural textures)
src/player.js   FPS controller, vitals, XP
src/weapons.js  weapon defs, view models, hitscan
src/enemies.js  merc/drone/boss AI, health bars, pickups
src/quests.js   job chain data + progression
src/upgrades.js ripperdoc shop data + purchase logic
src/hud.js      DOM HUD: vitals, minimap, dialogue, shop, toasts
src/fx.js       particles + tracers
src/audio.js    WebAudio-synthesized SFX, ambient, music
src/gamepad.js  Xbox controller polling and mapping
src/vehicle.js  hovercars: 3D flight, enter/exit, engine hooks
src/pov.js      five camera POVs with wall pull-in
test/smoke.mjs  headless logic test (node)
```
