import * as THREE from 'three';

export const WEAPON_DEFS = [
  {
    id: 'pistol', name: 'HI-POWER “KURO”', dmg: 34, rate: 0.16, mag: 12, reload: 1.05,
    auto: false, spread: 0.006, kick: 0.014, heatRate: 0.03, tracer: 0x8ef6ff, sfx: 'shoot'
  },
  {
    id: 'smg', name: 'R-S “WASP” SMG', dmg: 12, rate: 0.083, mag: 32, reload: 1.5,
    auto: true, spread: 0.011, kick: 0.006, heatRate: 0.09, tracer: 0xff9d3d, sfx: 'shoot_smg'
  }
];

const UP = new THREE.Vector3(0, 1, 0);

export class WeaponSystem {
  constructor(game) {
    this.game = game;
    this.state = WEAPON_DEFS.map((d, i) => ({
      mag: i === 0 ? d.mag : 0, reserve: i === 0 ? 48 : 96, unlocked: i === 0
    }));
    this.current = 0;
    this.cooldown = 0;
    this.reloading = false;
    this.reloadT = 0;
    this.heat = 0;
    this.kick = 0;
    this.swayX = 0;
    this.swayY = 0;
    this.flashT = 0;

    this.buildViewModels();

    this._dir = new THREE.Vector3();
    this._right = new THREE.Vector3();
    this._up = new THREE.Vector3();
    this._o = new THREE.Vector3();
    this._muzzle = new THREE.Vector3();
    this._c = new THREE.Vector3();
    this._end = new THREE.Vector3();
  }

  buildViewModels() {
    const cam = this.game.camera;
    const dark = new THREE.MeshStandardMaterial({ color: 0x161b26, roughness: 0.45, metalness: 0.75 });
    const darker = new THREE.MeshStandardMaterial({ color: 0x0e1219, roughness: 0.55, metalness: 0.6 });
    const cyan = new THREE.MeshBasicMaterial({ color: 0x3ef2ff, toneMapped: false });
    const pink = new THREE.MeshBasicMaterial({ color: 0xff2a6d, toneMapped: false });
    this.vms = [];
    this.muzzles = [];
    this.flashes = [];

    const pistol = new THREE.Group();
    const slide = new THREE.Mesh(new THREE.BoxGeometry(0.055, 0.06, 0.24), dark);
    slide.position.set(0, 0.02, -0.06);
    const grip = new THREE.Mesh(new THREE.BoxGeometry(0.05, 0.14, 0.07), darker);
    grip.position.set(0, -0.08, 0.05);
    grip.rotation.x = 0.25;
    const sight = new THREE.Mesh(new THREE.BoxGeometry(0.012, 0.012, 0.012), cyan);
    sight.position.set(0, 0.055, -0.16);
    const muzzle = new THREE.Object3D();
    muzzle.position.set(0, 0.03, -0.2);
    pistol.add(slide, grip, sight, muzzle);
    this.vms.push(pistol);
    this.muzzles.push(muzzle);

    const smg = new THREE.Group();
    const body = new THREE.Mesh(new THREE.BoxGeometry(0.06, 0.07, 0.36), dark);
    body.position.set(0, 0, -0.05);
    const supp = new THREE.Mesh(new THREE.CylinderGeometry(0.028, 0.028, 0.14, 8), darker);
    supp.rotation.x = Math.PI / 2;
    supp.position.set(0, 0.01, -0.28);
    const mag = new THREE.Mesh(new THREE.BoxGeometry(0.045, 0.16, 0.06), darker);
    mag.position.set(0, -0.1, 0.02);
    mag.rotation.x = 0.15;
    const stripe = new THREE.Mesh(new THREE.BoxGeometry(0.064, 0.012, 0.2), pink);
    stripe.position.set(0, 0.045, -0.05);
    const muzzle2 = new THREE.Object3D();
    muzzle2.position.set(0, 0.01, -0.37);
    smg.add(body, supp, mag, stripe, muzzle2);
    this.vms.push(smg);
    this.muzzles.push(muzzle2);

    const flashGeo = new THREE.PlaneGeometry(0.22, 0.22);
    const flashMat = new THREE.MeshBasicMaterial({
      color: 0xfff2c8, toneMapped: false, transparent: true, opacity: 0.95,
      blending: THREE.AdditiveBlending, depthWrite: false
    });
    for (const vm of this.vms) {
      const fl = new THREE.Mesh(flashGeo, flashMat);
      fl.visible = false;
      this.flashes.push(fl);
    }
    this.muzzles[0].add(this.flashes[0]);
    this.muzzles[1].add(this.flashes[1]);

    this.basePos = new THREE.Vector3(0.24, -0.2, -0.46);
    for (const vm of this.vms) {
      vm.position.copy(this.basePos);
      vm.traverse(o => { o.frustumCulled = false; });
      cam.add(vm);
    }
    this.vms[1].visible = false;
  }

  get def() { return WEAPON_DEFS[this.current]; }
  get st() { return this.state[this.current]; }

  addSway(mx, my) {
    this.swayX = Math.max(-0.03, Math.min(0.03, this.swayX - mx * 0.00045));
    this.swayY = Math.max(-0.03, Math.min(0.03, this.swayY + my * 0.00045));
  }

  update(dt, input) {
    this.cooldown -= dt;
    this.heat = Math.max(0, this.heat - dt * 0.8);
    this.kick = Math.max(0, this.kick - dt * 6);
    this.swayX *= Math.max(0, 1 - dt * 8);
    this.swayY *= Math.max(0, 1 - dt * 8);
    if (this.flashT > 0) {
      this.flashT -= dt;
      if (this.flashT <= 0) this.flashes[this.current].visible = false;
    }

    if (this.reloading) {
      this.reloadT -= dt;
      if (this.reloadT <= 0) {
        this.reloading = false;
        const take = Math.min(this.def.mag - this.st.mag, this.st.reserve);
        this.st.mag += take;
        this.st.reserve -= take;
      }
    }

    const wantFire = input.fireDown || (input.fire && this.def.auto);
    if (wantFire && !this.reloading && this.cooldown <= 0) {
      if (this.st.mag <= 0) this.reload();
      else this.shoot();
    }

    const vm = this.vms[this.current];
    vm.position.set(
      this.basePos.x + this.swayX,
      this.basePos.y + this.swayY + Math.sin(this.game.player.bobPhase) * 0.008 * this.game.player.bobAmt,
      this.basePos.z + this.kick * 0.09
    );
    let rotX = this.kick * 0.22;
    if (this.reloading) {
      const rl = 1 - this.reloadT / this.def.reload;
      rotX -= Math.sin(rl * Math.PI) * 0.7;
    }
    vm.rotation.x = rotX;
    vm.rotation.z = this.swayX * 1.5;
  }

  shoot() {
    const def = this.def;
    this.st.mag--;
    this.cooldown = def.rate;
    this.kick = Math.min(1, this.kick + 0.55);
    this.heat = Math.min(1, this.heat + def.heatRate);

    const player = this.game.player;
    const spread = def.spread + this.heat * 0.02 +
      (player.moving ? 0.004 : 0) + (player.sprinting ? 0.006 : 0);

    const head = this.game.player.head;
    head.updateWorldMatrix(true, false);
    head.getWorldPosition(this._o);
    head.getWorldDirection(this._dir).negate();
    this._right.crossVectors(this._dir, UP).normalize();
    this._up.crossVectors(this._right, this._dir);
    this._dir.addScaledVector(this._right, (Math.random() - 0.5) * spread * 2)
      .addScaledVector(this._up, (Math.random() - 0.5) * spread * 2)
      .normalize();

    const maxDist = 140;
    const worldHit = this.game.world.raycast(this._o, this._dir, maxDist);
    let tW = worldHit ? worldHit.t : maxDist;

    let hitEnemy = null, tE = Infinity;
    for (const e of this.game.enemies.enemies) {
      if (!e.alive) continue;
      this._c.set(
        e.group.position.x,
        e.group.position.y + e.hitY,
        e.group.position.z
      );
      const b = this._c.sub(this._o).dot(this._dir);
      if (b <= 0 || b > tW) continue;
      const d2 = this._c.lengthSq();
      const h2 = e.hitR * e.hitR - (d2 - b * b);
      if (h2 < 0) continue;
      const t = b - Math.sqrt(h2);
      if (t < tE) { tE = t; hitEnemy = e; }
    }

    const p = this.game.player;
    if (this.game.pov && this.game.pov.mode !== 'first' && !p.inCar && p.body) {
      p.bodyMuzzle.updateWorldMatrix(true, false);
      p.bodyMuzzle.getWorldPosition(this._muzzle);
    } else {
      this.muzzles[this.current].getWorldPosition(this._muzzle);
    }
    this.flashes[this.current].visible = true;
    this.flashes[this.current].rotation.z = Math.random() * Math.PI;
    this.flashT = 0.045;

    let killed = false;
    if (hitEnemy) {
      this._end.copy(this._o).addScaledVector(this._dir, tE);
      killed = this.game.enemies.damage(hitEnemy, def.dmg * player.dmgMult, this._end);
      this.game.hud?.hitmark(killed);
      this.game.audio?.sfx('hit', 0.9);
    } else if (worldHit) {
      this._end.copy(worldHit.point);
      this.game.fx?.sparks(this._end, 6, 0xffc788, 5);
    } else {
      this._end.copy(this._o).addScaledVector(this._dir, maxDist);
    }
    this.game.fx?.tracer(this._muzzle, this._end, def.tracer, 0.07, 0.8);
    this.game.audio?.sfx(def.sfx);
    this.game.hud?.updateAmmo();
    this.game.player.recoil(def.kick);
  }

  reload() {
    if (this.reloading || this.st.reserve <= 0 || this.st.mag >= this.def.mag) return;
    this.reloading = true;
    this.reloadT = this.def.reload;
    this.game.audio?.sfx('reload');
  }

  switchTo(i) {
    if (i === this.current || !this.state[i].unlocked) return;
    if (this.game.player.inCar) return;
    this.reloading = false;
    this.vms[this.current].visible = false;
    this.current = i;
    this.vms[i].visible = true;
    this.heat = 0;
    this.game.audio?.sfx('ui');
  }

  cycle(dir) {
    for (let k = 1; k >= -1; k--) {
      if (k === 0) continue;
    }
    const next = (this.current + dir + WEAPON_DEFS.length) % WEAPON_DEFS.length;
    if (this.state[next].unlocked) this.switchTo(next);
    else this.game.hud?.toast('WEAPON LOCKED — CLEAR MORE JOBS', '#ff4757');
  }

  unlock(i) {
    if (this.state[i].unlocked) return;
    this.state[i].unlocked = true;
    this.state[i].mag = WEAPON_DEFS[i].mag;
    this.game.hud?.toast('NEW HARDWARE // R-S “WASP” SMG — [2]', '#39ff88');
    this.game.audio?.sfx('quest');
  }

  addAmmo() {
    this.state[0].reserve += 24;
    this.state[1].reserve += 64;
    this.game.hud?.updateAmmo();
  }

  refillAmmo() {
    this.state[0].reserve = Math.max(this.state[0].reserve, 48);
    this.state[1].reserve = Math.max(this.state[1].reserve, 96);
    this.game.hud?.updateAmmo();
  }

  get spreadPx() {
    const p = this.game.player;
    return (this.def.spread + this.heat * 0.02 + (p.moving ? 0.004 : 0) + (p.sprinting ? 0.006 : 0)) * 2600 + 3;
  }
}
