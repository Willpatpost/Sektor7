import * as THREE from 'three';

const MAX = 26, ACC = 18, DRAG = 1.1, HOVER = 0.7, FLOOR = 0.5, CEIL = 95;
const clamp = THREE.MathUtils.clamp;

function angDiff(a, b) {
  let d = (b - a) % (Math.PI * 2);
  if (d > Math.PI) d -= Math.PI * 2;
  if (d < -Math.PI) d += Math.PI * 2;
  return d;
}

export class VehicleSystem {
  constructor(game) {
    this.game = game;
    this.cars = [];
    this.active = null;
    this.coasting = null;
    this.driving = false;
    this._f = new THREE.Vector3();
    this._r = new THREE.Vector3();
    this._a = new THREE.Vector3();
  }

  setup() {
    const spots = [
      { x: 2.5, z: 138, h: Math.PI, name: 'QUADRA “JINXIE”', body: 0x7a1fa2, glow: 0xff2a6d },
      { x: 52, z: 25, h: -Math.PI / 2, name: 'RAIJIN-8', body: 0x14405e, glow: 0x3ef2ff },
      { x: 25, z: -46, h: Math.PI, name: 'VOLGAR RAPTOR', body: 0x3a1414, glow: 0xffd60a },
      { x: 100, z: -125, h: -Math.PI / 2, name: 'MOTO-GHOST', body: 0x10362a, glow: 0x39ff88 }
    ];
    for (const s of spots) this.spawnCar(s);
  }

  spawnCar(s) {
    let { x, z } = s;
    const dir = Math.abs(Math.sin(s.h)) > 0.5 ? { dx: 0, dz: 1 } : { dx: 1, dz: 0 };
    for (let i = 0; i < 12 && this.game.world.pointBlocked(x, z, 2.4); i++) {
      x += dir.dx * 1.6;
      z += dir.dz * 1.6;
    }
    const g = this.buildMesh(s.body, s.glow);
    g.position.set(x, HOVER, z);
    g.rotation.y = s.h;
    this.game.scene.add(g);
    this.cars.push({
      group: g, heading: s.h, vel: new THREE.Vector3(), name: s.name,
      inner: g.userData.inner, thrusters: g.userData.thrusters
    });
  }

  buildMesh(body, glow) {
    const g = new THREE.Group();
    const inner = new THREE.Group();
    g.add(inner);
    const bodyMat = new THREE.MeshStandardMaterial({ color: body, roughness: 0.35, metalness: 0.75 });
    const glassMat = new THREE.MeshStandardMaterial({ color: 0x0a0f18, roughness: 0.08, metalness: 0.9 });
    const darkMat = new THREE.MeshStandardMaterial({ color: 0x0d1118, roughness: 0.7 });
    const hull = new THREE.Mesh(new THREE.BoxGeometry(1.9, 0.5, 4.4), bodyMat);
    hull.position.y = 0.55;
    const skirt = new THREE.Mesh(new THREE.BoxGeometry(2.0, 0.22, 4.2), darkMat);
    skirt.position.y = 0.28;
    const cabin = new THREE.Mesh(new THREE.BoxGeometry(1.68, 0.52, 2.1), glassMat);
    cabin.position.set(0, 1.05, -0.25);
    const nose = new THREE.Mesh(new THREE.BoxGeometry(1.7, 0.1, 0.08),
      new THREE.MeshBasicMaterial({ color: 0xdff6ff, toneMapped: false }));
    nose.position.set(0, 0.62, 2.22);
    const tail = new THREE.Mesh(new THREE.BoxGeometry(1.8, 0.12, 0.08),
      new THREE.MeshBasicMaterial({ color: 0xff2233, toneMapped: false }));
    tail.position.set(0, 0.68, -2.22);
    const under = new THREE.Mesh(new THREE.BoxGeometry(1.6, 0.05, 3.6),
      new THREE.MeshBasicMaterial({ color: glow, toneMapped: false, transparent: true, opacity: 0.9 }));
    under.position.y = 0.1;
    const fin = new THREE.Mesh(new THREE.BoxGeometry(0.08, 0.3, 0.9), bodyMat);
    fin.position.set(0.95, 1.05, -1.9);
    inner.add(hull, skirt, cabin, nose, tail, under, fin);

    const thrusters = [];
    for (const [px, pz] of [[-0.95, 1.4], [0.95, 1.4], [-0.95, -1.4], [0.95, -1.4]]) {
      const pod = new THREE.Mesh(new THREE.CylinderGeometry(0.16, 0.22, 0.3, 8), darkMat);
      pod.position.set(px, 0.22, pz);
      const jet = new THREE.Mesh(
        new THREE.ConeGeometry(0.16, 0.5, 8, 1, true),
        new THREE.MeshBasicMaterial({
          color: glow, toneMapped: false, transparent: true, opacity: 0.75,
          blending: THREE.AdditiveBlending, depthWrite: false
        })
      );
      jet.rotation.x = Math.PI;
      jet.position.set(px, 0.02, pz);
      inner.add(pod, jet);
      thrusters.push(jet);
    }
    g.userData.inner = inner;
    g.userData.thrusters = thrusters;
    return g;
  }

  enter(car) {
    if (this.driving) return;
    this.driving = true;
    this.active = car;
    this.coasting = null;
    const p = this.game.player;
    p.inCar = true;
    p.root.rotation.y = car.heading + Math.PI;
    for (const vm of this.game.weapons.vms) vm.visible = false;
    this.game.audio?.engineStart();
    this.game.hud?.toast(`THRUSTERS ONLINE — ${car.name}`, '#3ef2ff');
    this.game.audio?.sfx('ui');
  }

  exit() {
    if (!this.driving) return;
    const car = this.active;
    const p = this.game.player;
    const h = car.heading;
    const pos = car.group.position;
    const lx = Math.cos(h), lz = -Math.sin(h);
    let px = pos.x, pz = pos.z;
    const cands = [
      { x: pos.x + lx * 2.1, z: pos.z + lz * 2.1 },
      { x: pos.x - lx * 2.1, z: pos.z - lz * 2.1 },
      { x: pos.x - Math.sin(h) * 3.8, z: pos.z - Math.cos(h) * 3.8 }
    ];
    for (const c of cands) {
      if (!this.game.world.pointBlocked(c.x, c.z, 0.55) &&
          Math.abs(c.x) < 194 && Math.abs(c.z) < 194) {
        px = c.x;
        pz = c.z;
        break;
      }
    }
    p.root.position.set(px, Math.max(0.1, pos.y - 0.45), pz);
    p.inCar = false;
    p.head.position.y = 1.62;
    p.vy = 0;
    p.onGround = false;
    this.driving = false;
    this.active = null;
    this.coasting = car;
    this.game.weapons.vms[this.game.weapons.current].visible = true;
    this.game.audio?.engineStop();
  }

  forceExit() {
    if (!this.driving) return;
    const car = this.active;
    car.vel.set(0, 0, 0);
    this.driving = false;
    this.active = null;
    this.coasting = null;
    const p = this.game.player;
    p.inCar = false;
    p.head.position.y = 1.62;
    this.game.weapons.vms[this.game.weapons.current].visible = true;
    this.game.audio?.engineStop();
  }

  crash(intensity, at) {
    this.game.fx?.sparks(at, 14, 0xffc788, 9);
    this.game.audio?.sfx('boom', Math.min(1, intensity / 18), at);
    this.game.player.recoil(0.015);
  }

  collideAxis(car, axis, v) {
    if (v === 0) return;
    const pos = car.group.position;
    const vel = car.vel;
    const R = 1.55;
    for (const c of this.game.world.colliders) {
      if (c.h <= pos.y + 0.9) continue;
      if (pos.x > c.minX - R && pos.x < c.maxX + R && pos.z > c.minZ - R && pos.z < c.maxZ + R) {
        if (axis === 'x') {
          pos.x = v > 0 ? c.minX - R : c.maxX + R;
          if (Math.abs(vel.x) > 8) this.crash(Math.abs(vel.x), new THREE.Vector3(pos.x, pos.y + 0.8, pos.z));
          vel.x *= -0.3;
        } else {
          pos.z = v > 0 ? c.minZ - R : c.maxZ + R;
          if (Math.abs(vel.z) > 8) this.crash(Math.abs(vel.z), new THREE.Vector3(pos.x, pos.y + 0.8, pos.z));
          vel.z *= -0.3;
        }
      }
    }
  }

  update(dt, input) {
    const t = this.game.time;

    for (let i = 0; i < this.cars.length; i++) {
      const c = this.cars[i];
      if (c === this.active || c === this.coasting) continue;
      c.inner.position.y = Math.sin(t * 1.7 + i * 2.1) * 0.07;
      const pulse = 0.75 + Math.sin(t * 6 + i) * 0.25;
      for (const j of c.thrusters) j.scale.set(1, pulse, 1);
    }

    const car = this.driving ? this.active : this.coasting;
    if (!car) return;
    const pos = car.group.position;
    const vel = car.vel;

    let ix = 0, iz = 0, vert = 0;
    if (this.driving) {
      const k = input.keys;
      if (k.has('KeyW')) iz += 1;
      if (k.has('KeyS')) iz -= 1;
      if (k.has('KeyD')) ix += 1;
      if (k.has('KeyA')) ix -= 1;
      if (k.has('Space')) vert += 1;
      if (k.has('KeyC') || k.has('ShiftLeft')) vert -= 1;
      if (input.pad) {
        ix = clamp(ix + input.pad.x, -1, 1);
        iz = clamp(iz - input.pad.y, -1, 1);
        vert = clamp(vert + (input.pad.up ? 1 : 0) - (input.pad.down ? 1 : 0), -1, 1);
      }
    }

    const head = this.game.player.head;
    head.updateWorldMatrix(true, false);
    head.getWorldDirection(this._f).negate();
    const yaw = this.game.player.root.rotation.y;
    this._r.set(Math.cos(yaw), 0, -Math.sin(yaw));
    this._a.set(0, 0, 0);
    if (iz) this._a.addScaledVector(this._f, iz);
    if (ix) this._a.addScaledVector(this._r, ix);
    if (vert) this._a.y += vert;
    if (this._a.lengthSq() > 0) {
      this._a.normalize().multiplyScalar(ACC);
      vel.addScaledVector(this._a, dt);
    }
    vel.multiplyScalar(Math.exp(-DRAG * dt));
    if (vel.length() > MAX) vel.setLength(MAX);

    if (pos.y < HOVER && vert <= 0) {
      vel.y += (HOVER - pos.y) * 9 * dt;
    }

    pos.x += vel.x * dt;
    this.collideAxis(car, 'x', vel.x);
    pos.z += vel.z * dt;
    this.collideAxis(car, 'z', vel.z);

    const y0 = pos.y;
    pos.y += vel.y * dt;
    if (vel.y <= 0) {
      for (const c of this.game.world.colliders) {
        if (pos.x > c.minX - 1 && pos.x < c.maxX + 1 && pos.z > c.minZ - 1 && pos.z < c.maxZ + 1 &&
            pos.y < c.h + 0.55 && y0 >= c.h + 0.5) {
          if (vel.y < -7) this.crash(-vel.y, new THREE.Vector3(pos.x, c.h + 0.8, pos.z));
          pos.y = c.h + 0.55;
          vel.y = 0;
        }
      }
    }

    if (pos.y < FLOOR) {
      pos.y = FLOOR;
      if (vel.y < 0) vel.y = 0;
    }
    if (pos.y > CEIL) {
      pos.y = CEIL;
      if (vel.y > 0) vel.y = 0;
    }
    if (pos.x < -196 || pos.x > 196) {
      if (Math.abs(vel.x) > 8) this.crash(Math.abs(vel.x), new THREE.Vector3(pos.x, pos.y + 0.8, pos.z));
      pos.x = clamp(pos.x, -196, 196);
      vel.x *= -0.3;
    }
    if (pos.z < -196 || pos.z > 196) {
      if (Math.abs(vel.z) > 8) this.crash(Math.abs(vel.z), new THREE.Vector3(pos.x, pos.y + 0.8, pos.z));
      pos.z = clamp(pos.z, -196, 196);
      vel.z *= -0.3;
    }

    const hs = Math.hypot(vel.x, vel.z);
    if (hs > 2.5) {
      const target = Math.atan2(vel.x, vel.z);
      car.heading += angDiff(car.heading, target) * Math.min(1, dt * 4);
      car.group.rotation.y = car.heading;
    }
    const inner = car.inner;
    inner.rotation.x += (clamp(-vel.y * 0.03, -0.4, 0.4) - inner.rotation.x) * Math.min(1, dt * 5);
    inner.rotation.z += (clamp(-ix * 0.3, -0.35, 0.35) - inner.rotation.z) * Math.min(1, dt * 5);
    const sn = vel.length() / MAX;
    for (const j of car.thrusters) {
      j.scale.set(1, 0.8 + sn * 1.4 + Math.max(0, vert) * 0.8, 1);
    }

    if (vel.length() > 6 && pos.y < 2.6) {
      for (const e of this.game.enemies.enemies) {
        if (!e.alive) continue;
        const d = Math.hypot(e.group.position.x - pos.x, e.group.position.z - pos.z);
        if (d < 2) {
          this.game.fx?.explosion(new THREE.Vector3(e.group.position.x, 1, e.group.position.z));
          this.game.audio?.sfx('boom', 0.8, e.group.position);
          this.game.enemies.damage(e, 250, e.group.position);
          vel.multiplyScalar(0.86);
        }
      }
    }

    if (this.driving) {
      const p = this.game.player;
      this.game.audio?.engineUpdate(sn);
      const h = car.heading;
      const lx = Math.cos(h), lz = -Math.sin(h);
      p.root.position.set(
        pos.x + lx * 0.45 + Math.sin(h) * 0.15,
        pos.y - 0.4,
        pos.z + lz * 0.45 + Math.cos(h) * 0.15
      );
      p.head.position.y = 0.78 + Math.sin(t * 47) * 0.006 * sn;
      p.stamina = Math.min(p.maxStamina, p.stamina + 15 * dt);
      const cam = this.game.camera;
      const tf = 75 + sn * 12;
      if (Math.abs(cam.fov - tf) > 0.05) {
        cam.fov += (tf - cam.fov) * Math.min(1, dt * 4);
        cam.updateProjectionMatrix();
      }
    } else if (vel.length() < 0.3 && Math.abs(pos.y - HOVER) < 0.4) {
      vel.set(0, 0, 0);
      pos.y = HOVER;
      this.coasting = null;
    }
  }
}
