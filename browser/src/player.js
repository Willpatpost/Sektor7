import * as THREE from 'three';

export class Player {
  constructor(game) {
    this.game = game;
    this.root = new THREE.Object3D();
    this.head = new THREE.Object3D();
    this.head.position.y = 1.62;
    this.root.add(this.head);
    const p = game.world.pois.spawn;
    this.root.position.set(p.x, 0, p.z);
    game.scene.add(this.root);

    this.hp = 100;
    this.maxHp = 100;
    this.stamina = 100;
    this.maxStamina = 100;
    this.level = 1;
    this.xp = 0;
    this.eddies = 50;
    this.speedMult = 1;
    this.dmgMult = 1;
    this.radius = 0.55;
    this.vy = 0;
    this.onGround = true;
    this.pitch = 0;
    this.bobPhase = 0;
    this.bobAmt = 0;
    this.moving = false;
    this.sprinting = false;
    this.baseFov = 75;
    this.deaths = 0;
    this.inCar = false;
    this.buildBody();
  }

  buildBody() {
    const g = new THREE.Group();
    const cloth = new THREE.MeshStandardMaterial({ color: 0x1e2b36, roughness: 0.85 });
    const dark = new THREE.MeshStandardMaterial({ color: 0x12161d, roughness: 0.8 });
    const glow = new THREE.MeshBasicMaterial({ color: 0x3ef2ff, toneMapped: false });
    const legL = new THREE.Mesh(new THREE.BoxGeometry(0.16, 0.85, 0.2), dark);
    legL.position.set(-0.12, 0.42, 0);
    const legR = legL.clone();
    legR.position.x = 0.12;
    const torso = new THREE.Mesh(new THREE.BoxGeometry(0.5, 0.62, 0.28), cloth);
    torso.position.set(0, 1.12, 0);
    const stripe = new THREE.Mesh(new THREE.BoxGeometry(0.52, 0.06, 0.3), glow);
    stripe.position.set(0, 1.32, 0);
    const bodyHead = new THREE.Mesh(new THREE.BoxGeometry(0.24, 0.26, 0.26), dark);
    bodyHead.position.set(0, 1.58, 0);
    const visor = new THREE.Mesh(new THREE.BoxGeometry(0.26, 0.06, 0.06), glow);
    visor.position.set(0, 0.02, 0.13);
    bodyHead.add(visor);
    const gun = new THREE.Mesh(new THREE.BoxGeometry(0.08, 0.1, 0.5), dark);
    gun.position.set(0.3, 1.08, 0.25);
    const bodyMuzzle = new THREE.Object3D();
    bodyMuzzle.position.set(0.3, 1.12, 0.55);
    g.add(legL, legR, torso, stripe, bodyHead, gun, bodyMuzzle);
    g.visible = false;
    this.root.add(g);
    if (typeof document !== 'undefined' && this.game.world && this.game.world.blobTex) {
      this.blob = this.game.world.makeBlob(1.5);
      this.root.add(this.blob);
    }
    this.body = g;
    this.legL = legL;
    this.legR = legR;
    this.bodyHead = bodyHead;
    this.bodyMuzzle = bodyMuzzle;
  }

  get xpNext() { return 90 + this.level * 70; }

  onMouse(mx, my) {
    this.root.rotation.y -= mx * 0.0021;
    this.pitch = Math.max(-1.45, Math.min(1.45, this.pitch - my * 0.0021));
    this.head.rotation.x = this.pitch;
  }

  recoil(k) {
    this.pitch = Math.min(1.45, this.pitch + k);
    this.head.rotation.x = this.pitch;
  }

  eyePos(v) {
    return v.set(this.root.position.x, this.root.position.y + 1.62, this.root.position.z);
  }

  update(dt, input) {
    const k = input.keys;
    let f = 0, s = 0;
    if (k.has('KeyW')) f += 1;
    if (k.has('KeyS')) f -= 1;
    if (k.has('KeyD')) s += 1;
    if (k.has('KeyA')) s -= 1;
    let mag = 1;
    if (f === 0 && s === 0 && input.pad) {
      f = -input.pad.y;
      s = input.pad.x;
      const m = Math.hypot(f, s);
      if (m > 1e-4) {
        f /= m; s /= m;
        mag = Math.min(1, m);
      } else {
        f = 0; s = 0;
      }
    }
    const moving = f !== 0 || s !== 0;
    this.moving = moving;

    const wantSprint = k.has('ShiftLeft') || k.has('ShiftRight') || !!input.pad?.sprint;
    const sprinting = wantSprint && moving && this.stamina > 1;
    this.sprinting = sprinting;
    if (sprinting) this.stamina = Math.max(0, this.stamina - 22 * dt);
    else this.stamina = Math.min(this.maxStamina, this.stamina + 15 * dt);

    const speed = 5.2 * this.speedMult * mag * (sprinting ? 1.55 : 1);
    const yaw = this.root.rotation.y;
    const fx = -Math.sin(yaw), fz = -Math.cos(yaw);
    const rx = Math.cos(yaw), rz = -Math.sin(yaw);
    let dx = fx * f + rx * s, dz = fz * f + rz * s;
    const len = Math.hypot(dx, dz) || 1;
    dx = dx / len * speed * dt;
    dz = dz / len * speed * dt;
    this.game.world.collideMove(this.root.position, this.radius, dx, dz);

    if ((k.has('Space') || input.pad?.jump) && this.onGround) {
      this.vy = 5.4;
      this.onGround = false;
    }
    if (!this.onGround) {
      this.vy -= 15 * dt;
      this.root.position.y += this.vy * dt;
      if (this.root.position.y <= 0) {
        this.root.position.y = 0;
        this.vy = 0;
        this.onGround = true;
      }
    }

    this.bobPhase += dt * (sprinting ? 11 : 8) * (moving ? 1 : 0);
    this.bobAmt += ((moving && this.onGround ? 1 : 0) - this.bobAmt) * Math.min(1, dt * 8);
    this.head.position.y = 1.62 + Math.sin(this.bobPhase) * 0.05 * this.bobAmt;
    if (this.body && this.body.visible) {
      this.legL.rotation.x = Math.sin(this.bobPhase) * 0.55 * this.bobAmt;
      this.legR.rotation.x = -Math.sin(this.bobPhase) * 0.55 * this.bobAmt;
      this.bodyHead.rotation.x = this.pitch * 0.6;
    }

    const cam = this.game.camera;
    const targetFov = this.baseFov + (sprinting ? 7 : 0);
    if (Math.abs(cam.fov - targetFov) > 0.05) {
      cam.fov += (targetFov - cam.fov) * Math.min(1, dt * 6);
      cam.updateProjectionMatrix();
    }
  }

  damage(n) {
    if (this.hp <= 0) return false;
    this.hp -= n;
    this.game.hud?.damageFlash(n);
    this.game.audio?.sfx('hurt', 0.8);
    if (this.hp <= 0) {
      this.hp = 0;
      this.deaths++;
      this.game.onPlayerDeath?.();
      return true;
    }
    return false;
  }

  heal(n) {
    this.hp = Math.min(this.maxHp, this.hp + n);
  }

  addXP(n) {
    this.xp += n;
    let leveled = false;
    while (this.xp >= this.xpNext) {
      this.xp -= this.xpNext;
      this.level++;
      this.maxHp += 20;
      this.hp = this.maxHp;
      this.dmgMult += 0.05;
      leveled = true;
    }
    if (leveled) {
      this.game.hud?.toast(`LEVEL UP — LVL ${this.level} // +20 MAX HP`, '#39ff88');
      this.game.audio?.sfx('level');
    }
    return leveled;
  }

  reset() {
    const p = this.game.world.pois.spawn;
    this.root.position.set(p.x, 0, p.z);
    this.root.rotation.y = 0;
    this.pitch = 0;
    this.head.rotation.x = 0;
    this.hp = this.maxHp;
    this.vy = 0;
    this.onGround = true;
    this.stamina = this.maxStamina;
  }
}
