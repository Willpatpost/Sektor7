import * as THREE from 'three';

const CAP = 900;
const UP = new THREE.Vector3(0, 1, 0);

function makeDotTexture() {
  const c = document.createElement('canvas');
  c.width = c.height = 32;
  const g = c.getContext('2d');
  const grd = g.createRadialGradient(16, 16, 0, 16, 16, 16);
  grd.addColorStop(0, 'rgba(255,255,255,1)');
  grd.addColorStop(0.4, 'rgba(255,255,255,.75)');
  grd.addColorStop(1, 'rgba(255,255,255,0)');
  g.fillStyle = grd;
  g.fillRect(0, 0, 32, 32);
  const t = new THREE.CanvasTexture(c);
  t.colorSpace = THREE.SRGBColorSpace;
  return t;
}

export class FX {
  constructor(game) {
    this.game = game;
    this.pos = new Float32Array(CAP * 3);
    this.col = new Float32Array(CAP * 3);
    this.base = new Float32Array(CAP * 3);
    this.vel = new Float32Array(CAP * 3);
    this.life = new Float32Array(CAP);
    this.maxLife = new Float32Array(CAP);
    this.grav = new Float32Array(CAP);
    this.head = 0;
    for (let i = 0; i < CAP; i++) this.pos[i * 3 + 1] = -999;

    const geo = new THREE.BufferGeometry();
    geo.setAttribute('position', new THREE.BufferAttribute(this.pos, 3));
    geo.setAttribute('color', new THREE.BufferAttribute(this.col, 3));
    const mat = new THREE.PointsMaterial({
      size: 0.13, map: makeDotTexture(), vertexColors: true, transparent: true,
      depthWrite: false, blending: THREE.AdditiveBlending
    });
    this.points = new THREE.Points(geo, mat);
    this.points.frustumCulled = false;
    game.scene.add(this.points);

    const tgeo = new THREE.CylinderGeometry(0.02, 0.02, 1, 5, 1, true);
    this.tracers = [];
    for (let i = 0; i < 40; i++) {
      const m = new THREE.Mesh(tgeo, new THREE.MeshBasicMaterial({
        color: 0x53e6ff, transparent: true, opacity: 0, depthWrite: false,
        blending: THREE.AdditiveBlending, toneMapped: false
      }));
      m.visible = false;
      game.scene.add(m);
      this.tracers.push({ mesh: m, life: 0, maxLife: 1 });
    }
    this._c = new THREE.Color();
    this._d = new THREE.Vector3();
  }

  emit(x, y, z, vx, vy, vz, hex, life, grav) {
    const i = this.head;
    this.head = (this.head + 1) % CAP;
    this._c.set(hex);
    this.pos[i * 3] = x; this.pos[i * 3 + 1] = y; this.pos[i * 3 + 2] = z;
    this.vel[i * 3] = vx; this.vel[i * 3 + 1] = vy; this.vel[i * 3 + 2] = vz;
    this.base[i * 3] = this._c.r; this.base[i * 3 + 1] = this._c.g; this.base[i * 3 + 2] = this._c.b;
    this.life[i] = this.maxLife[i] = life;
    this.grav[i] = grav;
  }

  sparks(p, n, hex, speed = 7, grav = 16) {
    for (let i = 0; i < n; i++) {
      const a = Math.random() * Math.PI * 2;
      const e = (Math.random() - 0.28) * Math.PI;
      const s = speed * (0.35 + Math.random() * 0.75);
      this.emit(p.x, p.y, p.z,
        Math.cos(a) * Math.cos(e) * s, Math.sin(e) * s + 2, Math.sin(a) * Math.cos(e) * s,
        hex, 0.22 + Math.random() * 0.32, grav);
    }
  }

  explosion(p, hex = 0xff9d3d) {
    this.sparks(p, 46, hex, 15, 9);
    this.sparks(p, 22, 0xfff6e8, 6, 2);
  }

  tracer(from, to, hex = 0x53e6ff, life = 0.08, radius = 1) {
    let slot = this.tracers[0];
    for (const t of this.tracers) { if (!t.mesh.visible) { slot = t; break; } }
    const m = slot.mesh;
    this._d.subVectors(to, from);
    const len = this._d.length();
    if (len < 0.05) return;
    m.position.copy(from).addScaledVector(this._d, 0.5);
    m.quaternion.setFromUnitVectors(UP, this._d.multiplyScalar(1 / len));
    m.scale.set(radius, len, radius);
    m.material.color.setHex(hex);
    m.material.opacity = 1;
    m.visible = true;
    slot.life = slot.maxLife = life;
  }

  update(dt) {
    let any = false;
    for (let i = 0; i < CAP; i++) {
      if (this.life[i] <= 0) continue;
      any = true;
      this.life[i] -= dt;
      if (this.life[i] <= 0) {
        this.pos[i * 3 + 1] = -999;
        this.col[i * 3] = this.col[i * 3 + 1] = this.col[i * 3 + 2] = 0;
        continue;
      }
      this.vel[i * 3 + 1] -= this.grav[i] * dt;
      this.pos[i * 3] += this.vel[i * 3] * dt;
      this.pos[i * 3 + 1] += this.vel[i * 3 + 1] * dt;
      this.pos[i * 3 + 2] += this.vel[i * 3 + 2] * dt;
      if (this.pos[i * 3 + 1] < 0.02) { this.pos[i * 3 + 1] = 0.02; this.vel[i * 3 + 1] *= -0.35; }
      const f = this.life[i] / this.maxLife[i];
      this.col[i * 3] = this.base[i * 3] * f;
      this.col[i * 3 + 1] = this.base[i * 3 + 1] * f;
      this.col[i * 3 + 2] = this.base[i * 3 + 2] * f;
    }
    if (any) {
      this.points.geometry.attributes.position.needsUpdate = true;
      this.points.geometry.attributes.color.needsUpdate = true;
    }
    for (const t of this.tracers) {
      if (!t.mesh.visible) continue;
      t.life -= dt;
      if (t.life <= 0) { t.mesh.visible = false; continue; }
      t.mesh.material.opacity = t.life / t.maxLife;
    }
  }
}
