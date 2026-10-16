import * as THREE from 'three';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js';
import { OutputPass } from 'three/addons/postprocessing/OutputPass.js';
import { ShaderPass } from 'three/addons/postprocessing/ShaderPass.js';
import { World } from './world.js';
import { Player } from './player.js';
import { WeaponSystem } from './weapons.js';
import { EnemyManager } from './enemies.js';
import { QuestSystem } from './quests.js';
import { HUD } from './hud.js';
import { FX } from './fx.js';
import { AudioSys } from './audio.js';
import { GamepadSys } from './gamepad.js';
import { VehicleSystem } from './vehicle.js';
import { POVSystem } from './pov.js';

const canvas = document.getElementById('game');
const renderer = new THREE.WebGLRenderer({
  canvas, antialias: true, powerPreference: 'high-performance', preserveDrawingBuffer: true
});
renderer.setPixelRatio(Math.min(window.devicePixelRatio, 1.75));
renderer.setSize(window.innerWidth, window.innerHeight);
renderer.toneMapping = THREE.ACESFilmicToneMapping;
renderer.toneMappingExposure = 1.15;

const scene = new THREE.Scene();
const camera = new THREE.PerspectiveCamera(75, window.innerWidth / window.innerHeight, 0.05, 600);

const game = {
  scene, camera, renderer,
  state: 'menu',
  time: 0,
  stats: { kills: 0, eddiesEarned: 0, startedAt: 0 },
  upgradeTiers: {},
  interactables: [],
  won: false,
  fallbackLook: false,
  mouse: { x: window.innerWidth / 2, y: window.innerHeight / 2 },

  toast(msg, color) { this.hud?.toast(msg, color); },
  addEddies(n) {
    this.player.eddies += n;
    this.stats.eddiesEarned += Math.max(0, n);
  },
  addInteractable(it) { this.interactables.push(it); },
  removeInteractable(it) {
    const i = this.interactables.indexOf(it);
    if (i >= 0) this.interactables.splice(i, 1);
  },
  onEnemyKilled(e) {
    this.stats.kills++;
    const xp = e.type === 'boss' ? 300 : e.type === 'drone' ? 40 : 50;
    this.player.addXP(xp);
    this.quests.onKill(e.type);
  },
  onPlayerDeath() {
    game.state = 'dead';
    game.vehicle?.forceExit();
    document.exitPointerLock();
    game.audio.sfx('death');
    game.hud.show('death');
  },
  onBossTaunt() {
    game.hud.dialogue('RAZORTAIL', [
      'The plaza rat from Kaneda. Vex sent you?',
      'I\u2019ve eaten better with my morning synth-café.'
    ]);
  },
  win() {
    game.state = 'won';
    game.won = true;
    document.exitPointerLock();
    game.audio.sfx('win');
    const t = Math.round(game.time - game.stats.startedAt);
    const mm = String(Math.floor(t / 60)).padStart(2, '0');
    const ss = String(t % 60).padStart(2, '0');
    game.hud.showWin({
      time: `${mm}:${ss}`,
      kills: game.stats.kills,
      eddiesEarned: game.stats.eddiesEarned,
      level: game.player.level
    });
  },
  closeShop() {
    game.hud.hide('shop');
    game.state = 'playing';
    lockPointer();
  }
};

game.world = new World(game);
game.world.computeLayout();
game.world.buildVisuals();

game.player = new Player(game);
scene.add(camera);

game.fx = new FX(game);
game.enemies = new EnemyManager(game);
game.weapons = new WeaponSystem(game);
game.quests = new QuestSystem(game);
game.hud = new HUD(game);
game.pov = new POVSystem(game);
game.audio = new AudioSys();
game.audio.game = game;
game.gamepad = new GamepadSys(game);
game.vehicle = new VehicleSystem(game);
game.vehicle.setup();

const composer = new EffectComposer(renderer, new THREE.WebGLRenderTarget(
  window.innerWidth, window.innerHeight, { samples: 4, type: THREE.HalfFloatType }
));
composer.addPass(new RenderPass(scene, camera));
composer.addPass(new UnrealBloomPass(
  new THREE.Vector2(window.innerWidth, window.innerHeight), 0.9, 0.6, 0.62
));
composer.addPass(new OutputPass());

const GradeShader = {
  uniforms: {
    tDiffuse: { value: null },
    uTime: { value: 0 },
    uSpeed: { value: 0 }
  },
  vertexShader: `
    varying vec2 vUv;
    void main() {
      vUv = uv;
      gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
    }`,
  fragmentShader: `
    uniform sampler2D tDiffuse;
    uniform float uTime;
    uniform float uSpeed;
    varying vec2 vUv;
    float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
    void main() {
      vec2 c = vUv - 0.5;
      float r2 = dot(c, c);
      float ab = 0.0016 * (0.35 + r2 * 2.4) * (1.0 + uSpeed * 2.2);
      vec2 dir = normalize(c + vec2(1e-6, 1e-6));
      vec3 col;
      col.r = texture2D(tDiffuse, vUv + dir * ab).r;
      col.g = texture2D(tDiffuse, vUv).g;
      col.b = texture2D(tDiffuse, vUv - dir * ab).b;
      col += vec3(0.010, 0.018, 0.032) * (1.0 - col);
      col *= vec3(1.045, 1.0, 0.975);
      float vig = smoothstep(0.95, 0.32, r2 * 1.55);
      col *= mix(0.7, 1.0, vig);
      float g = hash(vUv * vec2(1920.0, 1080.0) + fract(uTime) * 61.7);
      col += (g - 0.5) * 0.032;
      gl_FragColor = vec4(col, 1.0);
    }`
};
const gradePass = new ShaderPass(GradeShader);
composer.addPass(gradePass);
game.composer = composer;

function buildMarker() {
  const c = document.createElement('canvas');
  c.width = c.height = 64;
  const g = c.getContext('2d');
  g.translate(32, 32);
  g.rotate(Math.PI / 4);
  g.strokeStyle = '#ffd60a';
  g.lineWidth = 5;
  g.shadowColor = '#ffd60a';
  g.shadowBlur = 14;
  g.strokeRect(-14, -14, 28, 28);
  g.fillStyle = 'rgba(255,214,10,.3)';
  g.fillRect(-14, -14, 28, 28);
  const tex = new THREE.CanvasTexture(c);
  tex.colorSpace = THREE.SRGBColorSpace;
  const group = new THREE.Group();
  const sprite = new THREE.Sprite(new THREE.SpriteMaterial({ map: tex, transparent: true, depthTest: false }));
  sprite.renderOrder = 999;
  sprite.scale.setScalar(1.5);
  sprite.position.y = 3;
  const beam = new THREE.Mesh(
    new THREE.CylinderGeometry(0.45, 0.45, 46, 10, 1, true),
    new THREE.MeshBasicMaterial({
      color: 0xffd60a, transparent: true, opacity: 0.1, depthWrite: false,
      blending: THREE.AdditiveBlending, toneMapped: false
    })
  );
  beam.position.y = 23;
  const ring = new THREE.Mesh(
    new THREE.TorusGeometry(1.1, 0.06, 6, 24),
    new THREE.MeshBasicMaterial({ color: 0xffd60a, toneMapped: false, transparent: true, opacity: 0.85 })
  );
  ring.rotation.x = Math.PI / 2;
  ring.position.y = 0.15;
  group.add(sprite, beam, ring);
  group.visible = false;
  scene.add(group);
  return { group, sprite, ring };
}
const marker = buildMarker();

game.addInteractable({
  id: 'ripperdoc',
  x: game.world.ripperdoc.x,
  z: game.world.ripperdoc.z,
  radius: 2.8,
  prompt: 'RIPPERDOC — DOC CHROME',
  action: () => openShop()
});

function openShop() {
  if (game.state !== 'playing') return;
  game.state = 'shop';
  document.exitPointerLock();
  game.hud.showShop();
  game.audio.sfx('ui');
}

function lockPointer() {
  if (game.fallbackLook) return;
  try {
    const r = canvas.requestPointerLock();
    if (r && r.catch) r.catch(() => enableFallback());
  } catch (_) {
    enableFallback();
  }
}

function enableFallback() {
  if (game.fallbackLook) return;
  game.fallbackLook = true;
  game.hud?.toast('MOUSE LOCK BLOCKED — EDGE-STEER MODE ON', '#ffd60a');
  const fh = document.getElementById('fallbackhint');
  if (fh) fh.style.display = 'block';
}

function edgeSteer(dt) {
  const w = window.innerWidth, h = window.innerHeight;
  const nx = (game.mouse.x - w / 2) / (w / 2);
  const ny = (game.mouse.y - h / 2) / (h / 2);
  const DZ = 0.3;
  const steer = (v) => {
    const a = Math.abs(v);
    if (a <= DZ) return 0;
    return Math.sign(v) * (a - DZ) / (1 - DZ);
  };
  const sx = steer(nx), sy = steer(ny);
  if (sx || sy) {
    game.player.onMouse(sx * 1150 * dt, sy * 1150 * dt);
    game.weapons.addSway(sx * 200 * dt, sy * 200 * dt);
  }
}

/* ---------------- input ---------------- */

const input = { keys: new Set(), fire: false, fireDown: false, mouseFire: false, padFire: false, pad: null };

function onInteractKey() {
  if (game.hud.dialogueActive()) {
    game.hud.advanceDialogue();
    return;
  }
  if (game.player.inCar) {
    game.vehicle.exit();
    return;
  }
  const car = nearestCar();
  if (car) {
    game.vehicle.enter(car);
    return;
  }
  const it = scanInteract();
  if (it) it.action();
}

function nearestCar() {
  const p = game.player.root.position;
  let best = null;
  let bd = 3.4;
  for (const c of game.vehicle.cars) {
    const d = Math.hypot(c.group.position.x - p.x, c.group.position.z - p.z);
    if (d < bd) {
      bd = d;
      best = c;
    }
  }
  return best;
}

window.addEventListener('keydown', (e) => {
  if (e.repeat) return;
  input.keys.add(e.code);
  const confirmKey = e.code === 'KeyE' || e.code === 'Enter';
  if (confirmKey) {
    if (game.state === 'playing') onInteractKey();
    else if (game.state === 'shop') game.closeShop();
    else if (game.state === 'paused') resumeGame();
    else if (game.state === 'dead') respawn();
    else if (game.state === 'won') roamContinue();
    else if (game.state === 'menu') startGame();
    return;
  }
  if (e.code === 'Escape') {
    if (game.state === 'shop') game.closeShop();
    else if (game.state === 'playing') pauseGame();
    return;
  }
  if (game.state === 'playing') {
    if (e.code === 'KeyR') game.weapons.reload();
    if (e.code === 'Digit1') game.weapons.switchTo(0);
    if (e.code === 'Digit2') game.weapons.switchTo(1);
    if (e.code === 'KeyV') game.pov.cycle();
  }
});
window.addEventListener('keyup', (e) => input.keys.delete(e.code));
window.addEventListener('mousemove', (e) => {
  game.mouse.x = e.clientX;
  game.mouse.y = e.clientY;
  if (document.pointerLockElement === canvas && game.state === 'playing') {
    game.player.onMouse(e.movementX, e.movementY);
    game.weapons.addSway(e.movementX, e.movementY);
  }
});
window.addEventListener('mousedown', (e) => {
  if (e.button !== 0) return;
  if (game.state === 'playing' &&
      (document.pointerLockElement === canvas || game.fallbackLook)) {
    input.mouseFire = true;
    input.fireDown = true;
  } else if (game.state === 'playing' && !document.pointerLockElement &&
             !game.gamepad.connected && !game.fallbackLook) {
    lockPointer();
  }
});
window.addEventListener('mouseup', (e) => {
  if (e.button === 0) input.mouseFire = false;
});
window.addEventListener('wheel', (e) => {
  if (game.state === 'playing' && document.pointerLockElement === canvas) {
    game.weapons.cycle(e.deltaY > 0 ? 1 : -1);
  }
});
canvas.addEventListener('contextmenu', (e) => e.preventDefault());

document.addEventListener('pointerlockchange', () => {
  if (document.pointerLockElement === canvas) {
    if (game.state === 'paused') {
      game.state = 'playing';
      game.hud.hide('pause');
    }
  } else {
    input.mouseFire = false;
    if (game.state === 'playing') {
      game.state = 'paused';
      game.hud.show('pause');
    }
  }
});
document.addEventListener('pointerlockerror', () => enableFallback());
document.addEventListener('mouseout', (e) => {
  if (!e.relatedTarget) {
    game.mouse.x = window.innerWidth / 2;
    game.mouse.y = window.innerHeight / 2;
  }
});
window.addEventListener('blur', () => {
  game.mouse.x = window.innerWidth / 2;
  game.mouse.y = window.innerHeight / 2;
});

window.addEventListener('resize', () => {
  camera.aspect = window.innerWidth / window.innerHeight;
  camera.updateProjectionMatrix();
  renderer.setSize(window.innerWidth, window.innerHeight);
  composer.setSize(window.innerWidth, window.innerHeight);
});

/* ---------------- flow ---------------- */

function startGame() {
  game.audio.init();
  game.audio.resume();
  game.hud.hide('menu');
  game.state = 'playing';
  game.stats.startedAt = game.time;
  lockPointer();
  setTimeout(() => {
    if (game.state === 'playing' && !document.pointerLockElement &&
        !game.gamepad.connected && !game.fallbackLook) {
      enableFallback();
    }
  }, 800);
  setTimeout(() => {
    if (game.state !== 'menu') game.quests.begin(0);
  }, 900);
}

function respawn() {
  game.player.reset();
  game.enemies.resetAggro();
  game.hud.hide('death');
  game.state = 'playing';
  lockPointer();
}

function pauseGame() {
  if (game.state !== 'playing') return;
  game.state = 'paused';
  game.hud.show('pause');
  if (document.pointerLockElement) document.exitPointerLock();
}

function resumeGame() {
  game.hud.hide('pause');
  game.state = 'playing';
  lockPointer();
}

function roamContinue() {
  game.hud.hide('win');
  game.state = 'playing';
  lockPointer();
}

const resumeAudio = () => game.audio.resume();
window.addEventListener('mousedown', resumeAudio);
window.addEventListener('keydown', resumeAudio);

document.getElementById('startbtn').onclick = startGame;
document.getElementById('resumebtn').onclick = resumeGame;
document.getElementById('respawnbtn').onclick = respawn;
document.getElementById('roambtn').onclick = roamContinue;

window.__S7 = { game, startGame, enableFallback, pauseGame };

function scanInteract() {
  const p = game.player.root.position;
  if (game.player.inCar) {
    game.hud.setInteract('EXIT VEHICLE');
    return null;
  }
  let best = null;
  let bd = Infinity;
  for (const it of game.interactables) {
    const d = Math.hypot(it.x - p.x, it.z - p.z);
    if (d < it.radius && d < bd) {
      bd = d;
      best = it;
    }
  }
  if (best) {
    game.hud.setInteract(best.prompt);
    return best;
  }
  const car = nearestCar();
  game.hud.setInteract(car ? `ENTER ${car.name}` : null);
  return car ? { car: true } : null;
}

function updateMarker(dt) {
  const mp = game.quests.markerPos();
  if (mp && game.state === 'playing') {
    marker.group.visible = true;
    marker.group.position.set(mp.x, 0, mp.z);
    marker.sprite.position.y = 3 + Math.sin(game.time * 2) * 0.3;
    marker.sprite.scale.setScalar(1.5 + Math.sin(game.time * 3) * 0.15);
    marker.ring.rotation.z += dt * 1.5;
    const p = game.player.root.position;
    game.hud.setMarkerDist(Math.hypot(mp.x - p.x, mp.z - p.z));
  } else {
    marker.group.visible = false;
    game.hud.setMarkerDist(null);
  }
}

/* ---------------- loop ---------------- */

function pollGamepad(dt) {
  const gp = game.gamepad;
  gp.update();
  input.pad = gp.connected
    ? { x: gp.moveX, y: gp.moveY, sprint: gp.sprint, jump: gp.jump, up: gp.up, down: gp.down }
    : null;
  input.padFire = gp.connected && gp.fire;
  if (gp.connected && gp.fire && !gp.prevFire) input.fireDown = true;
  gp.prevFire = gp.fire;
  if (!gp.connected) return;

  if (game.state === 'playing') {
    if (gp.lookX || gp.lookY) {
      game.player.onMouse(gp.lookX * 1240 * dt, gp.lookY * 1240 * dt);
      game.weapons.addSway(gp.lookX * 260 * dt, gp.lookY * 260 * dt);
    }
    if (gp.edge(2)) game.weapons.reload();
    if (gp.edge(3)) game.weapons.cycle(1);
    if (gp.edge(1)) onInteractKey();
    if (gp.edge(9)) pauseGame();
    if (gp.edge(14)) game.pov.cycle();
  } else if (game.state === 'menu') {
    if (gp.edge(0)) startGame();
  } else if (game.state === 'paused') {
    if (gp.edge(0) || gp.edge(9)) resumeGame();
  } else if (game.state === 'dead') {
    if (gp.edge(0)) respawn();
  } else if (game.state === 'won') {
    if (gp.edge(0)) roamContinue();
  } else if (game.state === 'shop') {
    if (gp.edge(1)) {
      game.closeShop();
      return;
    }
    if (gp.edge(12) || (gp.moveY < -0.5 && !gp.stickUp)) game.hud.shopMove(-1);
    if (gp.edge(13) || (gp.moveY > 0.5 && !gp.stickDown)) game.hud.shopMove(1);
    if (gp.edge(0)) game.hud.shopConfirm();
    gp.stickUp = gp.moveY < -0.5;
    gp.stickDown = gp.moveY > 0.5;
  }
}

let last = performance.now();
renderer.setAnimationLoop(() => {
  const now = performance.now();
  const dt = Math.min(0.05, (now - last) / 1000);
  last = now;
  game.time += dt;

  pollGamepad(dt);
  input.fire = input.mouseFire || input.padFire;

  if (game.state === 'playing') {
    if (game.fallbackLook && document.pointerLockElement !== canvas) edgeSteer(dt);
    if (!game.player.inCar) game.player.update(dt, input);
    game.vehicle.update(dt, input);
    game.weapons.update(dt, input);
    game.enemies.update(dt);
    game.quests.update(dt);
    scanInteract();
  }
  input.fireDown = false;
  game.pov.update(dt);
  gradePass.uniforms.uTime.value = game.time;
  const gsSpeed = game.player.inCar
    ? (game.vehicle.active ? game.vehicle.active.vel.length() : 0)
    : (game.player.sprinting ? 14 : (game.player.moving ? 5 : 0));
  gradePass.uniforms.uSpeed.value = Math.min(1, gsSpeed / 24);

  game.world.update(dt);
  game.fx.update(dt);
  game.audio.update();
  updateMarker(dt);
  game.hud.update(dt);
  composer.render();
});
