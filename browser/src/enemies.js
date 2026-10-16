import * as THREE from 'three';

const rand = (a, b) => a + Math.random() * (b - a);

function angDiff(a, b) {
  let d = (b - a) % (Math.PI * 2);
  if (d > Math.PI) d -= Math.PI * 2;
  if (d < -Math.PI) d += Math.PI * 2;
  return d;
}

export class EnemyManager {
  constructor(game) {
    this.game = game;
    this.enemies = [];
    this.pickups = [];
    this._m = new THREE.Vector3();
    this._t = new THREE.Vector3();
  }

  /* ---------- spawning ---------- */

  spawnMerc(x, z, opts = {}) {
    const boss = !!opts.boss;
    const e = {
      type: boss ? 'boss' : 'merc', boss,
      hp: opts.hp ?? (boss ? 600 : 100),
      aggressive: opts.aggressive ?? false,
      speed: boss ? 3.0 : rand(1.5, 2.0),
      dmg: boss ? [6, 9] : [5, 8],
      acc: boss ? 0.55 : 0.48,
      burstMax: boss ? 5 : 3,
      state: 'patrol', home: { x, z }, wp: { x, z }, wpWait: rand(0, 2),
      alive: true, gone: false, deathT: 0, losT: 0, lastSeen: null,
      nextBurst: rand(1, 2), burstN: 0, nextShot: 0,
      strafePh: rand(0, 9), taunted: false, aggroT: -99,
      hitY: boss ? 1.45 : 1.05, hitR: boss ? 1.15 : 0.8,
      animPh: rand(0, 9), fallDir: Math.random() < 0.5 ? 1 : -1
    };
    e.maxHp = e.hp;

    const g = new THREE.Group();
    const cloth = new THREE.MeshStandardMaterial({ color: boss ? 0x33202a : 0x2a2433, roughness: 0.85 });
    const dark = new THREE.MeshStandardMaterial({ color: 0x14181f, roughness: 0.8 });
    const glow = new THREE.MeshBasicMaterial({ color: boss ? 0xff2233 : 0x3ef2ff, toneMapped: false });

    const legL = new THREE.Mesh(new THREE.BoxGeometry(0.16, 0.85, 0.2), dark);
    legL.position.set(-0.12, 0.42, 0);
    const legR = legL.clone();
    legR.position.x = 0.12;
    const torso = new THREE.Mesh(new THREE.BoxGeometry(0.52, 0.62, 0.3), cloth);
    torso.position.set(0, 1.12, 0);
    const stripe = new THREE.Mesh(new THREE.BoxGeometry(0.54, 0.06, 0.32), glow);
    stripe.position.set(0, 1.3, 0);
    const head = new THREE.Mesh(new THREE.BoxGeometry(0.24, 0.26, 0.26), dark);
    head.position.set(0, 1.58, 0);
    const visor = new THREE.Mesh(new THREE.BoxGeometry(0.26, 0.06, 0.06), glow);
    visor.position.set(0, 1.6, 0.15);
    const gun = new THREE.Mesh(new THREE.BoxGeometry(0.08, 0.1, 0.5), dark);
    gun.position.set(0.3, 1.1, 0.3);
    const muzzle = new THREE.Object3D();
    muzzle.position.set(0.3, 1.12, 0.58);
    g.add(legL, legR, torso, stripe, head, visor, gun, muzzle);
    if (boss) g.scale.setScalar(1.35);
    g.position.set(x, 0, z);
    this.game.scene.add(g);

    e.group = g;
    e.legL = legL; e.legR = legR; e.gunMuzzle = muzzle;
    this.makeBar(e);
    if (typeof document !== 'undefined' && this.game.world && this.game.world.blobTex) {
      e.blob = this.game.world.makeBlob(1.3);
      g.add(e.blob);
    }
    this.enemies.push(e);
    return e;
  }

  spawnBoss(x, z) {
    return this.spawnMerc(x, z, { boss: true, aggressive: true, hp: 600 });
  }

  spawnDrone(x, z, opts = {}) {
    const e = {
      type: 'drone',
      hp: opts.hp ?? 40,
      aggressive: opts.aggressive ?? true,
      anchor: { x, z },
      state: 'patrol', alive: true, gone: false, deathT: 0,
      hoverPh: rand(0, 9), orbitPh: rand(0, Math.PI * 2),
      fireT: rand(1, 2), vy: 0, sparkT: 0, aggroT: -99,
      hitY: 0, hitR: 0.75
    };
    e.maxHp = e.hp;

    const g = new THREE.Group();
    const body = new THREE.Mesh(
      new THREE.OctahedronGeometry(0.48, 0),
      new THREE.MeshStandardMaterial({ color: 0x1a202c, roughness: 0.5, metalness: 0.7, flatShading: true })
    );
    const eye = new THREE.Mesh(
      new THREE.SphereGeometry(0.15, 8, 6),
      new THREE.MeshBasicMaterial({ color: 0xff3344, toneMapped: false })
    );
    eye.position.set(0, 0, 0.42);
    const ring = new THREE.Mesh(
      new THREE.TorusGeometry(0.32, 0.035, 6, 18),
      new THREE.MeshBasicMaterial({ color: 0x3ef2ff, toneMapped: false })
    );
    ring.rotation.x = Math.PI / 2;
    ring.position.y = -0.42;
    const ant = new THREE.Mesh(
      new THREE.CylinderGeometry(0.015, 0.015, 0.5, 4),
      new THREE.MeshStandardMaterial({ color: 0x14181f })
    );
    ant.position.y = 0.45;
    g.add(body, eye, ring, ant);
    g.position.set(x, 7, z);
    this.game.scene.add(g);

    e.group = g;
    e.ring = ring;
    this.makeBar(e);
    this.enemies.push(e);
    return e;
  }

  makeBar(e) {
    if (typeof document === 'undefined') return;
    const c = document.createElement('canvas');
    c.width = 64;
    c.height = 8;
    const tex = new THREE.CanvasTexture(c);
    const spr = new THREE.Sprite(new THREE.SpriteMaterial({ map: tex, depthTest: false, transparent: true }));
    spr.scale.set(e.boss ? 1.7 : 1.15, e.boss ? 0.24 : 0.16, 1);
    spr.position.y = e.type === 'drone' ? 0.9 : (e.boss ? 2.9 : 2.15);
    e.group.add(spr);
    e.bar = { spr, ctx: c.getContext('2d'), tex };
    this.drawBar(e);
  }

  drawBar(e) {
    if (!e.bar) return;
    const g = e.bar.ctx;
    g.clearRect(0, 0, 64, 8);
    g.fillStyle = 'rgba(6,10,16,.9)';
    g.fillRect(0, 0, 64, 8);
    const f = Math.max(0, e.hp / e.maxHp);
    g.fillStyle = `hsl(${f * 115},90%,55%)`;
    g.fillRect(1, 1, 62 * f, 6);
    e.bar.tex.needsUpdate = true;
  }

  /* ---------- combat ---------- */

  damage(e, n, hitPos) {
    if (!e.alive) return false;
    e.hp -= n;
    e.aggroT = this.game.time;
    this.drawBar(e);
    if (e.state === 'patrol') {
      e.state = 'combat';
      e.lastSeen = { x: this.game.player.root.position.x, z: this.game.player.root.position.z };
    }
    if (e.hp <= 0) {
      this.die(e, hitPos);
      return true;
    }
    return false;
  }

  die(e, hitPos) {
    e.alive = false;
    e.deathT = 0;
    e.state = 'dead';
    if (e.bar) e.bar.spr.visible = false;
    if (e.blob) e.blob.visible = false;
    const p = e.group.position;
    this.game.fx?.sparks(hitPos || p, e.type === 'drone' ? 16 : 20, e.type === 'drone' ? 0x53e6ff : 0xff9d3d, 9);
    this.game.audio?.sfx('kill', 1, p);
    if (e.boss) this.game.audio?.sfx('boom', 1, p);

    const val = e.boss ? 300 : (e.type === 'drone' ? Math.round(rand(20, 40)) : Math.round(rand(30, 60)));
    this.spawnPickup(p.x, p.z, 'eddies', val);
    if (Math.random() < 0.35) this.spawnPickup(p.x + rand(-1, 1), p.z + rand(-1, 1), 'ammo', 0);
    if (Math.random() < 0.2) this.spawnPickup(p.x + rand(-1, 1), p.z + rand(-1, 1), 'stim', 0);

    this.game.onEnemyKilled?.(e);
  }

  spawnPickup(x, z, kind, val) {
    let mesh;
    if (kind === 'eddies') {
      mesh = new THREE.Mesh(new THREE.OctahedronGeometry(0.22, 0),
        new THREE.MeshBasicMaterial({ color: 0x39ff88, toneMapped: false }));
    } else if (kind === 'ammo') {
      mesh = new THREE.Mesh(new THREE.BoxGeometry(0.3, 0.22, 0.3),
        new THREE.MeshBasicMaterial({ color: 0xffd60a, toneMapped: false }));
    } else {
      mesh = new THREE.Mesh(new THREE.OctahedronGeometry(0.2, 0),
        new THREE.MeshBasicMaterial({ color: 0xff4d6d, toneMapped: false }));
    }
    mesh.position.set(x, 0.55, z);
    this.game.scene.add(mesh);
    this.pickups.push({ mesh, kind, val, ph: rand(0, 9), ttl: 45 });
  }

  resetAggro() {
    for (const e of this.enemies) {
      if (!e.alive) continue;
      e.state = 'patrol';
      e.losT = 0;
      e.burstN = 0;
      e.nextBurst = rand(1, 2);
      e.fireT = rand(1, 2);
      e.aggroT = -99;
    }
  }

  countAlive(type) {
    let n = 0;
    for (const e of this.enemies) if (e.alive && e.type === type) n++;
    return n;
  }

  /* ---------- update ---------- */

  update(dt) {
    const p = this.game.player.root.position;

    for (const e of this.enemies) {
      if (e.gone) continue;
      if (!e.alive) {
        e.deathT += dt;
        if (e.type === 'drone') {
          e.vy -= 22 * dt;
          e.group.position.y += e.vy * dt;
          e.group.rotation.z += dt * 8;
          e.sparkT -= dt;
          if (e.sparkT <= 0) {
            e.sparkT = 0.09;
            this.game.fx?.sparks(e.group.position, 3, 0x53e6ff, 4);
          }
          if (e.group.position.y < 0.5) {
            this.game.fx?.explosion(e.group.position);
            this.game.audio?.sfx('boom', 1, e.group.position);
            this.game.scene.remove(e.group);
            e.gone = true;
          }
        } else {
          e.group.rotation.z = Math.max(-1.5, Math.min(1.5, e.group.rotation.z + e.fallDir * dt * 5));
          if (e.deathT > 0.5) e.group.position.y -= dt * 1.1;
          if (e.deathT > 2.4) {
            this.game.scene.remove(e.group);
            e.gone = true;
          }
        }
        continue;
      }

      const ex = e.group.position.x, ez = e.group.position.z;
      const distP = Math.hypot(p.x - ex, p.z - ez);
      const hunted = (this.game.time - e.aggroT) < 8;

      if (e.type === 'drone') this.droneAI(e, dt, p, distP, hunted);
      else this.mercAI(e, dt, p, distP, hunted);
    }

    const ground = this.enemies.filter(e => e.alive && e.type !== 'drone');
    for (let i = 0; i < ground.length; i++) {
      for (let j = i + 1; j < ground.length; j++) {
        const a = ground[i].group.position, b = ground[j].group.position;
        const dx = b.x - a.x, dz = b.z - a.z;
        const d = Math.hypot(dx, dz);
        if (d > 0.001 && d < 1.3) {
          const push = (1.3 - d) * 0.5;
          const ux = dx / d, uz = dz / d;
          this.game.world.collideMove(a, 0.5, -ux * push, -uz * push);
          this.game.world.collideMove(b, 0.5, ux * push, uz * push);
        }
      }
    }

    for (const e of ground) {
      const a = e.group.position;
      const dx = p.x - a.x, dz = p.z - a.z;
      const d = Math.hypot(dx, dz);
      if (d > 0.001 && d < 1.1) {
        this.game.world.collideMove(a, 0.5, -dx / d * 0.4, -dz / d * 0.4);
      }
    }

    this.updatePickups(dt);
    this.enemies = this.enemies.filter(e => !e.gone);
  }

  mercAI(e, dt, p, distP, hunted) {
    const pos = e.group.position;
    const los = !this.game.world.segBlocked(pos.x, 1.55, pos.z, p.x, 1.6, p.z);

    let seen = false;
    if (hunted) seen = distP < 70 && los;
    else if (e.aggressive) seen = distP < 60 && los;
    else if (distP < 38 && los) {
      const toP = Math.atan2(p.x - pos.x, p.z - pos.z);
      seen = Math.abs(angDiff(e.group.rotation.y, toP)) < 1.35;
    }

    if (seen) {
      if (e.state === 'patrol') this.game.audio?.sfx('alert', 0.7, pos);
      e.state = 'combat';
      e.lastSeen = { x: p.x, z: p.z };
      e.losT = 0;
    } else if (e.state === 'combat') {
      e.losT += dt;
      if (e.losT > 2.5) e.state = 'seek';
    }

    if (e.boss && !e.taunted && distP < 30) {
      e.taunted = true;
      this.game.onBossTaunt?.();
    }

    let moveX = 0, moveZ = 0, moveMag = 0;

    if (e.state === 'patrol') {
      const dx = e.wp.x - pos.x, dz = e.wp.z - pos.z;
      const d = Math.hypot(dx, dz);
      if (d < 0.4) {
        e.wpWait -= dt;
        if (e.wpWait <= 0) {
          e.wp = this.game.world.findClearPos(e.home.x, e.home.z, 2, 9);
          e.wpWait = rand(0.5, 3);
        }
      } else {
        moveX = dx / d; moveZ = dz / d;
        moveMag = e.speed * 0.8;
        e.group.rotation.y = Math.atan2(moveX, moveZ);
      }
    } else if (e.state === 'seek' && e.lastSeen) {
      const dx = e.lastSeen.x - pos.x, dz = e.lastSeen.z - pos.z;
      const d = Math.hypot(dx, dz);
      if (d < 2) {
        e.state = 'patrol';
        e.wp = this.game.world.findClearPos(e.home.x, e.home.z, 2, 9);
      } else {
        moveX = dx / d; moveZ = dz / d;
        moveMag = e.speed;
        e.group.rotation.y = Math.atan2(moveX, moveZ);
      }
    } else if (e.state === 'combat') {
      const toP = Math.atan2(p.x - pos.x, p.z - pos.z);
      e.group.rotation.y += angDiff(e.group.rotation.y, toP) * Math.min(1, dt * 7);

      if (distP > 17) {
        moveX = (p.x - pos.x) / distP;
        moveZ = (p.z - pos.z) / distP;
        moveMag = e.speed;
      } else if (distP < 8) {
        moveX = (pos.x - p.x) / distP;
        moveZ = (pos.z - p.z) / distP;
        moveMag = e.speed * 0.8;
      } else {
        const s = Math.sin(this.game.time * 1.1 + e.strafePh);
        moveX = (p.z - pos.z) / distP * s;
        moveZ = -(p.x - pos.x) / distP * s;
        moveMag = e.speed * 0.85;
      }

      if (los && distP < 46) {
        e.nextBurst -= dt;
        if (e.nextBurst <= 0 && e.burstN <= 0) {
          e.burstN = e.burstMax;
          e.nextShot = 0;
          e.nextBurst = rand(1.4, 2.3);
        }
        if (e.burstN > 0) {
          e.nextShot -= dt;
          if (e.nextShot <= 0) {
            this.mercFire(e, p, distP);
            e.burstN--;
            e.nextShot = 0.13;
          }
        }
      }
    }

    if (moveMag > 0) {
      this.game.world.collideMove(pos, 0.5, moveX * moveMag * dt, moveZ * moveMag * dt);
      e.animPh += dt * 7 * moveMag;
      e.legL.rotation.x = Math.sin(e.animPh) * 0.55;
      e.legR.rotation.x = -Math.sin(e.animPh) * 0.55;
    } else {
      e.legL.rotation.x *= 1 - Math.min(1, dt * 10);
      e.legR.rotation.x *= 1 - Math.min(1, dt * 10);
    }
  }

  mercFire(e, p, distP) {
    e.gunMuzzle.getWorldPosition(this._m);
    const acc = e.acc * Math.max(0.3, Math.min(1, 1.25 - distP / 50)) *
      (this.game.player.sprinting ? 0.75 : 1);
    const hit = Math.random() < acc;
    this._t.set(
      p.x + (hit ? rand(-0.2, 0.2) : rand(-1.8, 1.8)),
      p.y + 1.45 + (hit ? rand(-0.2, 0.2) : rand(-0.5, 0.9)),
      p.z + (hit ? rand(-0.2, 0.2) : rand(-1.8, 1.8))
    );
    this.game.fx?.tracer(this._m, this._t, e.boss ? 0xff2233 : 0xff5a5a, 0.09, 0.7);
    this.game.fx?.sparks(this._m, 2, 0xffc788, 3);
    this.game.audio?.sfx('eshoot', 1, this._m);
    if (hit) this.game.player.damage(Math.round(rand(e.dmg[0], e.dmg[1])));
  }

  droneAI(e, dt, p, distP, hunted) {
    const pos = e.group.position;
    const t = this.game.time;
    const combat = hunted || e.aggressive || distP < 55;
    const alt = 6.8 + Math.sin(t * 0.9 + e.hoverPh) * 0.9;

    let tx, tz;
    if (combat) {
      e.orbitPh += dt * 0.55;
      tx = p.x + Math.sin(e.orbitPh) * 13;
      tz = p.z + Math.cos(e.orbitPh) * 13;
    } else {
      tx = e.anchor.x + Math.sin(t * 0.3 + e.hoverPh) * 6;
      tz = e.anchor.z + Math.cos(t * 0.25 + e.hoverPh) * 6;
    }
    const dx = tx - pos.x, dz = tz - pos.z;
    const d = Math.hypot(dx, dz) || 1;
    const spd = combat ? 6.5 : 3;
    this.game.world.collideMove(pos, 0.5, dx / d * spd * dt, dz / d * spd * dt, 6);
    pos.y += (alt - pos.y) * Math.min(1, dt * 2);
    e.group.rotation.y = Math.atan2(p.x - pos.x, p.z - pos.z);
    e.ring.rotation.z += dt * 4;

    if (combat && distP < 42 && !this.game.world.segBlocked(pos.x, pos.y, pos.z, p.x, 1.6, p.z)) {
      e.fireT -= dt;
      if (e.fireT <= 0) {
        e.fireT = rand(1.3, 1.8);
        this._m.copy(pos);
        this._m.z += 0.4;
        const acc = 0.5 * Math.max(0.3, Math.min(1, 1.2 - distP / 50));
        const hit = Math.random() < acc;
        this._t.set(
          p.x + (hit ? rand(-0.2, 0.2) : rand(-2, 2)),
          1.5 + (hit ? rand(-0.2, 0.2) : rand(-0.4, 1)),
          p.z + (hit ? rand(-0.2, 0.2) : rand(-2, 2))
        );
        this.game.fx?.tracer(this._m, this._t, 0x53e6ff, 0.12, 1.1);
        this.game.audio?.sfx('laser', 1, this._m);
        if (hit) this.game.player.damage(4);
      }
    }
  }

  updatePickups(dt) {
    const p = this.game.player.root.position;
    this.pickups = this.pickups.filter(pk => {
      pk.ttl -= dt;
      pk.ph += dt * 3;
      if (pk.ttl <= 0) {
        this.game.scene.remove(pk.mesh);
        return false;
      }
      pk.mesh.position.y = 0.55 + Math.sin(pk.ph) * 0.18;
      pk.mesh.rotation.y += dt * 2.2;
      if (pk.ttl < 6) pk.mesh.visible = Math.sin(pk.ph * 6) > -0.4;
      const d = Math.hypot(p.x - pk.mesh.position.x, p.z - pk.mesh.position.z);
      if (d < 1.7 && p.y < 2) {
        this.collect(pk);
        return false;
      }
      return true;
    });
  }

  collect(pk) {
    this.game.scene.remove(pk.mesh);
    this.game.audio?.sfx('pickup');
    if (pk.kind === 'eddies') {
      this.game.addEddies?.(pk.val);
      this.game.hud?.toast(`¢ +${pk.val}`, '#39ff88');
    } else if (pk.kind === 'ammo') {
      this.game.weapons?.addAmmo();
      this.game.hud?.toast('AMMO +24 ⁄ +64', '#ffd60a');
    } else {
      this.game.player.heal(30);
      this.game.hud?.toast('STIM +30 HP', '#ff4d6d');
    }
  }
}
