import * as THREE from 'three';
import { World } from '../src/world.js';
import { Player } from '../src/player.js';
import { EnemyManager } from '../src/enemies.js';
import { WeaponSystem } from '../src/weapons.js';
import { QuestSystem } from '../src/quests.js';
import { VehicleSystem } from '../src/vehicle.js';
import { POVSystem } from '../src/pov.js';

let passed = 0, failed = 0;
function ok(cond, msg) {
  if (cond) { passed++; console.log('  ✓ ' + msg); }
  else { failed++; console.error('  ✗ ' + msg); }
}

const scene = new THREE.Scene();
const camera = new THREE.PerspectiveCamera(75, 1, 0.05, 600);

const game = {
  scene, camera, time: 0, state: 'playing',
  stats: { kills: 0, eddiesEarned: 0, startedAt: 0 },
  upgradeTiers: {},
  interactables: [],
  won: false,
  toast() {},
  addEddies(n) { this.player.eddies += n; this.stats.eddiesEarned += n; },
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
  onBossTaunt() {},
  win() { this.won = true; },
  closeShop() {}
};

console.log('— world layout');
const world = new World(game);
world.computeLayout();
game.world = world;
ok(world.colliders.length > 80, `colliders generated (${world.colliders.length})`);
ok(world.pois.spawn && world.pois.plaza && world.pois.lockup && world.pois.market && world.pois.boss, 'all POIs present');
ok(!world.pointBlocked(world.pois.spawn.x, world.pois.spawn.z, 0.55), 'spawn point is clear');
for (const k of ['plaza', 'market', 'boss']) {
  ok(!world.pointBlocked(world.pois[k].x, world.pois[k].z, 0.55), `${k} POI is clear`);
}
const b = world.colliders.find(c => c.h > 10 && c.mm);
ok(!!b, 'found a tall building collider for LOS test');
if (b) {
  const cz = (b.minZ + b.maxZ) / 2;
  ok(world.segBlocked(b.minX - 5, 1.6, cz, (b.minX + b.maxX) / 2, 1.6, cz), 'segBlocked detects building');
  const o = new THREE.Vector3(b.minX - 5, 1.6, cz);
  const hit = world.raycast(o, new THREE.Vector3(1, 0, 0), 100);
  ok(hit && hit.t > 3 && hit.t < 6, `raycast hits building (t=${hit ? hit.t.toFixed(2) : 'miss'})`);
}

console.log('— player movement');
const player = new Player(game);
game.player = player;
const keys = new Set(['KeyW']);
const input = { keys, fire: false, fireDown: false };
const px0 = player.root.position.x, pz0 = player.root.position.z;
for (let i = 0; i < 240; i++) { player.update(1 / 60, input); game.time += 1 / 60; }
const moved = Math.hypot(player.root.position.x - px0, player.root.position.z - pz0);
ok(moved > 3, `player walks forward (${moved.toFixed(1)}m)`);
ok(Math.abs(player.root.position.x) <= 197 && Math.abs(player.root.position.z) <= 197, 'player stays in bounds');
ok(!world.pointBlocked(player.root.position.x, player.root.position.z, 0.5), 'player not inside geometry');
keys.add('ShiftLeft');
for (let i = 0; i < 100; i++) player.update(1 / 60, input);
ok(player.stamina < 100, `sprint drains stamina (${player.stamina.toFixed(0)})`);
keys.clear();

console.log('— enemies');
const enemies = new EnemyManager(game);
game.enemies = enemies;
player.root.position.set(0, 0, 140);
player.root.rotation.y = 0;
player.hp = 100000;
let damageCalls = 0;
const origDamage = player.damage.bind(player);
player.damage = (n) => { damageCalls++; return origDamage(n); };

const merc = enemies.spawnMerc(0, 118, { aggressive: true });
const drone = enemies.spawnDrone(0, 128, { aggressive: true });
ok(merc.alive && drone.alive, 'merc + drone spawned on street');
for (let i = 0; i < 600; i++) { enemies.update(1 / 60); game.time += 1 / 60; }
ok(merc.state === 'combat', 'merc enters combat vs player');
ok(damageCalls > 0, `enemies fire at player (${damageCalls} hits)`);
ok(Number.isFinite(merc.group.position.x) && Number.isFinite(drone.group.position.y), 'enemy positions finite');
ok(!world.pointBlocked(merc.group.position.x, merc.group.position.z, 0.4), 'merc not inside geometry');

console.log('— weapons');
const weapons = new WeaponSystem(game);
game.weapons = weapons;
scene.add(camera);
merc.group.position.set(0, 0, 118);
camera.position.set(0, 1.62, 140);
camera.lookAt(0, 1.05, 118);
camera.updateMatrixWorld(true);
const hp0 = merc.hp;
weapons.update(1 / 60, { keys: new Set(), fire: true, fireDown: true });
ok(merc.hp < hp0, `hitscan damages merc (${(hp0 - merc.hp).toFixed(1)} dmg)`);
weapons.reload();
ok(weapons.reloading, 'reload starts');
for (let i = 0; i < 80; i++) weapons.update(1 / 60, { keys: new Set(), fire: false, fireDown: false });
ok(!weapons.reloading && weapons.st.mag === weapons.def.mag, 'reload completes');
weapons.unlock(1);
ok(weapons.state[1].unlocked, 'SMG unlock works');
weapons.switchTo(1);
ok(weapons.current === 1, 'weapon switch works');

console.log('— quest chain');
const quests = new QuestSystem(game);
game.quests = quests;
quests.begin(0);
ok(enemies.countAlive('merc') >= 4, `job 1 spawned plaza mercs (${enemies.countAlive('merc')} alive)`);
for (const e of enemies.enemies.filter(e => e.alive && e.type === 'merc')) {
  enemies.damage(e, 99999, e.group.position);
}
ok(!quests.active && quests.idx === 0, 'job 1 auto-completes on kills');
ok(player.eddies >= 450, `job rewards wired (¢${player.eddies})`);
quests.update(5);
ok(quests.idx === 1 && quests.active, 'job 2 begins after delay');
ok(game.interactables.some(i => i.id === 'shard'), 'shard interactable registered');
quests.collectShard();
ok(!quests.active, 'job 2 completes on shard pickup');
quests.update(5);
ok(quests.idx === 2 && enemies.countAlive('drone') >= 4, `job 3 spawns drones (${enemies.countAlive('drone')})`);
for (const e of enemies.enemies.filter(e => e.alive && e.type === 'drone')) {
  enemies.damage(e, 99999, e.group.position);
}
quests.update(5);
ok(quests.idx === 3, 'job 4 (boss) begins');
const boss = enemies.enemies.find(e => e.alive && e.type === 'boss');
ok(!!boss, 'boss spawned');
enemies.damage(boss, 999999, boss.group.position);
ok(game.won, 'killing boss triggers win');
ok(quests.allDone, 'quest chain marked all done');
ok(player.level > 1, `XP leveled player (LVL ${player.level})`);
ok(game.stats.kills >= 12, `kill stats tracked (${game.stats.kills})`);

console.log('— vehicle (hover flight)');
const vehicle = new VehicleSystem(game);
game.vehicle = vehicle;
vehicle.setup();
ok(vehicle.cars.length >= 4, `hovercars spawned (${vehicle.cars.length})`);
const car = vehicle.cars[0];
ok(!world.pointBlocked(car.group.position.x, car.group.position.z, 1.4), 'hovercar parked clear');
ok(Math.abs(car.group.position.y - 0.7) < 0.1, 'hovercar idles at hover altitude');
camera.position.set(0, 1.6, 0);
camera.rotation.set(0, 0, 0);
camera.updateMatrixWorld(true);
player.root.rotation.y = 0;
player.root.position.set(car.group.position.x + 1.2, 0, car.group.position.z);
vehicle.enter(car);
ok(player.inCar && vehicle.driving, 'enter hovercar');
const cz0 = car.group.position.z;
keys.add('KeyW');
for (let i = 0; i < 300; i++) { vehicle.update(1 / 60, input); game.time += 1 / 60; }
ok(car.vel.length() > 6, `hovercar accelerates (${car.vel.length().toFixed(1)} m/s)`);
ok(car.group.position.z < cz0 - 10, `flies toward look direction (${(cz0 - car.group.position.z).toFixed(1)}m)`);
keys.clear();
keys.add('Space');
for (let i = 0; i < 180; i++) vehicle.update(1 / 60, input);
ok(car.group.position.y > 4, `ascends freely (${car.group.position.y.toFixed(1)}m)`);
keys.clear();
keys.add('KeyC');
for (let i = 0; i < 300; i++) vehicle.update(1 / 60, input);
ok(car.group.position.y < 1.6, `descends to hover altitude (${car.group.position.y.toFixed(1)}m)`);
keys.clear();
keys.add('KeyD');
const cx0 = car.group.position.x;
for (let i = 0; i < 150; i++) vehicle.update(1 / 60, input);
ok(car.group.position.x > cx0 + 1, `strafes right (${(car.group.position.x - cx0).toFixed(1)}m)`);
keys.clear();
const roof = world.colliders.find(c => c.h > 12 && c.h < 45 && c.mm);
ok(!!roof, 'found mid-rise building for roof test');
if (roof) {
  car.group.position.set((roof.minX + roof.maxX) / 2, roof.h + 8, (roof.minZ + roof.maxZ) / 2);
  car.vel.set(0, 0, 0);
  keys.add('KeyC');
  let roofLanded = false;
  for (let i = 0; i < 400 && !roofLanded; i++) {
    vehicle.update(1 / 60, input);
    roofLanded = Math.abs(car.group.position.y - (roof.h + 0.55)) < 0.3 && car.vel.y === 0;
  }
  ok(roofLanded, `lands and rests on rooftop (y=${car.group.position.y.toFixed(2)} vs ${roof.h + 0.55})`);
  keys.clear();
}
car.group.position.set(0, 1, 180);
car.vel.set(0, 0, 30);
for (let i = 0; i < 60; i++) vehicle.update(1 / 60, input);
ok(Math.abs(car.group.position.z) <= 196.5 && Math.abs(car.vel.z) < 12,
  `boundary wall stops hovercar (z=${car.group.position.z.toFixed(1)}, vz=${car.vel.z.toFixed(1)})`);
car.group.position.set(2.5, 0.9, 100);
car.vel.set(0, 0, -15);
const m2 = enemies.spawnMerc(2.5, 90, {});
m2.wp = { x: 2.5, z: 90 };
let ranOver = false;
for (let i = 0; i < 120 && !ranOver; i++) { vehicle.update(1 / 60, input); ranOver = !m2.alive; }
ok(ranOver, 'low-altitude run-over kills enemy');
car.vel.set(0, 0, 0);
car.group.position.set(2.5, 12, 90);
vehicle.exit();
ok(!player.inCar && !vehicle.driving, 'exit hovercar mid-air');
let fellLanded = false;
const fallInput = { keys: new Set(), fire: false, fireDown: false };
for (let i = 0; i < 400 && !fellLanded; i++) {
  player.update(1 / 60, fallInput);
  if (player.root.position.y === 0 && player.onGround) fellLanded = true;
}
ok(fellLanded, 'player falls from altitude and lands safely');

console.log('— pov');
game.pov = new POVSystem(game);
ok(game.pov.modes.length === 5, 'five POV modes defined');
player.root.position.set(0, 0, 150);
player.root.rotation.y = 0;
player.pitch = 0;
player.head.rotation.x = 0;
player.head.position.y = 1.62;
player.inCar = false;
const headState = () => {
  player.head.updateWorldMatrix(true, false);
  const e = player.head.matrixWorld.elements;
  return { x: e[12], y: e[13], z: e[14], fx: -e[8], fy: -e[9], fz: -e[10] };
};
const camDist = () => {
  const h = headState();
  return Math.hypot(camera.position.x - h.x, camera.position.y - h.y, camera.position.z - h.z);
};
for (let i = 0; i < 40; i++) game.pov.update(1 / 60);
ok(camDist() < 0.3, `first person camera at eye (${camDist().toFixed(2)}m)`);
ok(!player.body.visible, 'body hidden in first person');
game.pov.cycle();
for (let i = 0; i < 40; i++) game.pov.update(1 / 60);
let hs = headState();
let d = camDist();
let dot = (camera.position.x - hs.x) * hs.fx + (camera.position.y - hs.y) * hs.fy + (camera.position.z - hs.z) * hs.fz;
ok(d > 2 && d < 3.4, `behind-close offset (${d.toFixed(2)}m)`);
ok(dot < -1.5, 'behind-close camera sits behind the player');
ok(player.body.visible, 'player body visible in third person');
game.pov.cycle();
for (let i = 0; i < 40; i++) game.pov.update(1 / 60);
d = camDist();
ok(d > 4.8 && d < 6.6, `behind-far offset (${d.toFixed(2)}m)`);
game.pov.cycle();
for (let i = 0; i < 40; i++) game.pov.update(1 / 60);
hs = headState();
d = camDist();
dot = (camera.position.x - hs.x) * hs.fx + (camera.position.y - hs.y) * hs.fy + (camera.position.z - hs.z) * hs.fz;
ok(d > 1.8 && d < 3.4, `front-close offset (${d.toFixed(2)}m)`);
ok(dot > 1.5, 'front-close camera sits in front of the player');
game.pov.cycle();
for (let i = 0; i < 40; i++) game.pov.update(1 / 60);
d = camDist();
ok(d > 4.2 && d < 6.2, `front-far offset (${d.toFixed(2)}m)`);
game.pov.cycle();
for (let i = 0; i < 40; i++) game.pov.update(1 / 60);
ok(camDist() < 0.3, `cycles back to first person (${camDist().toFixed(2)}m)`);
const povWall = world.colliders.find(c => c.h > 20 && c.mm);
ok(!!povWall, 'found tall building for camera pull-in test');
if (povWall) {
  game.pov.cycle();
  player.root.position.set(povWall.minX - 1.2, 0, (povWall.minZ + povWall.maxZ) / 2);
  player.root.rotation.y = Math.PI / 2;
  player.pitch = 0;
  player.head.rotation.x = 0;
  for (let i = 0; i < 40; i++) game.pov.update(1 / 60);
  d = camDist();
  ok(d < 1.5 && d > 0.2, `camera pulls in near walls (${d.toFixed(2)}m)`);
}

console.log(`\n${passed} passed, ${failed} failed`);
process.exit(failed ? 1 : 0);
