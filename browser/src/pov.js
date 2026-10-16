import * as THREE from 'three';

const UP = new THREE.Vector3(0, 1, 0);

export const POV_MODES = [
  { id: 'first', label: 'FIRST PERSON' },
  { id: 'behindClose', label: 'THIRD PERSON — CLOSE' },
  { id: 'behindFar', label: 'THIRD PERSON — FAR' },
  { id: 'frontClose', label: 'FRONT — CLOSE' },
  { id: 'frontFar', label: 'FRONT — FAR' }
];

export class POVSystem {
  constructor(game) {
    this.game = game;
    this.modes = POV_MODES;
    this.idx = 0;
    this._eye = new THREE.Vector3();
    this._dir = new THREE.Vector3();
    this._ray = new THREE.Vector3();
    this._pos = new THREE.Vector3();
    this._q = new THREE.Quaternion();
    this._qt = new THREE.Quaternion();
    this._m4 = new THREE.Matrix4();
  }

  get mode() { return this.modes[this.idx].id; }
  get front() { return this.mode.startsWith('front'); }
  get label() { return this.modes[this.idx].label; }

  cycle() {
    this.idx = (this.idx + 1) % this.modes.length;
    this.game.hud?.toast(`POV — ${this.label}`, '#9fd8ff');
    this.game.audio?.sfx('ui');
  }

  update(dt) {
    const g = this.game;
    const p = g.player;
    const cam = g.camera;
    const head = p.head;
    head.updateWorldMatrix(true, false);
    this._eye.setFromMatrixPosition(head.matrixWorld);
    head.getWorldQuaternion(this._q);
    head.getWorldDirection(this._dir).negate();

    const m = this.mode;
    let dist = 0, lift = 0, isFront = false;
    if (m === 'behindClose') { dist = 2.8; lift = 0.35; }
    else if (m === 'behindFar') { dist = 6.0; lift = 1.1; }
    else if (m === 'frontClose') { dist = 2.6; lift = 0.15; isFront = true; }
    else if (m === 'frontFar') { dist = 5.5; lift = 0.7; isFront = true; }

    if (dist === 0) {
      this._pos.copy(this._eye);
    } else {
      const ray = isFront ? this._dir : this._ray.copy(this._dir).negate();
      let d = dist;
      const hit = g.world.raycast(this._eye, ray, dist + 0.4);
      if (hit && hit.t < dist + 0.4) d = Math.max(0.4, hit.t - 0.35);
      this._pos.copy(this._eye).addScaledVector(ray, d)
        .addScaledVector(UP, lift * (d / dist));
    }

    const k = Math.min(1, dt * 18);
    cam.position.lerp(this._pos, k);
    if (isFront) {
      this._m4.lookAt(cam.position, this._eye, UP);
      this._qt.setFromRotationMatrix(this._m4);
      cam.quaternion.slerp(this._qt, k);
    } else {
      cam.quaternion.slerp(this._q, k);
    }

    const first = m === 'first';
    const showBody = !first && !p.inCar;
    if (p.body) p.body.visible = showBody;
    if (p.blob) p.blob.visible = showBody;
    if (g.weapons) {
      for (let i = 0; i < g.weapons.vms.length; i++) {
        g.weapons.vms[i].visible = first && !p.inCar && i === g.weapons.current;
      }
    }
  }
}
