import * as THREE from 'three';

const CELL = 50, BLOCK = 34, GRID = 8, HALF = GRID * CELL / 2;
const BOUND = 196;
const IN = HALF - CELL / 2;

const rand = (a, b) => a + Math.random() * (b - a);
const pick = (arr) => arr[(Math.random() * arr.length) | 0];

const SIGN_WORDS = ['RAMEN', 'ノODLE', 'CYBERWARE', 'MOTEL', 'BAR 電', 'X-CHANGE', 'PAWN+', 'TECH 24H',
  'HASHI', 'CLUB', 'KARA0KE', 'HOTEL', 'SUSHI', 'VOLT', 'DOKI', 'NEON', '餃子', 'GIRLS'];
const NEON = ['#3ef2ff', '#ff2a6d', '#ffd60a', '#39ff88', '#b026ff', '#ff5f1f'];
const FACADES = ['#10141d', '#141820', '#0f1712', '#151119'];

const SPECIAL = {
  spawnCell: [3, 6], plazaCell: [5, 4], lockupCell: [1, 3], marketCell: [4, 2], bossCell: [6, 1]
};

function cellCenter(col, row) {
  return { x: -HALF + col * CELL + CELL / 2, z: -HALF + row * CELL + CELL / 2 };
}

export class World {
  constructor(game) {
    this.game = game;
    this.colliders = [];
    this.bldSpecs = [];
    this.signSpecs = [];
    this.stripSpecs = [];
    this.carSpecs = [];
    this.kioskSpecs = [];
    this.lampPos = [];
    this.dashSpecs = [];
    this.pylonSpecs = [];
    this.pedSpecs = [];
    this.flyerSpecs = [];
    this.decorSpecs = [];
    this.pois = {};
    this.ripperdoc = null;
    this.built = false;
  }

  addCollider(minX, maxX, minZ, maxZ, h, mm = false) {
    this.colliders.push({ minX, maxX, minZ, maxZ, h, mm });
  }

  boxCollider(x, z, w, d, h, mm = true) {
    this.addCollider(x - w / 2, x + w / 2, z - d / 2, z + d / 2, h, mm);
  }

  computeLayout() {
    const special = new Set(Object.values(SPECIAL).map(c => c.join(',')));

    for (let col = 0; col < GRID; col++) {
      for (let row = 0; row < GRID; row++) {
        const key = col + ',' + row;
        const c = cellCenter(col, row);
        if (key === SPECIAL.plazaCell.join(',')) this.layoutPlaza(c);
        else if (key === SPECIAL.lockupCell.join(',')) this.layoutLockup(c);
        else if (key === SPECIAL.marketCell.join(',')) this.layoutMarket(c);
        else if (key === SPECIAL.bossCell.join(',')) this.layoutBoss(c);
        else if (key === SPECIAL.spawnCell.join(',')) this.layoutSpawnBlock(c);
        else this.layoutBlock(c);
        if (!special.has(key)) this.pedSpecs.push(...this.makePeds(c));
      }
    }

    this.layoutStreets();
    this.layoutPerimeter();

    this.pois = {
      spawn: { x: 0, z: 146 },
      plaza: cellCenter(...SPECIAL.plazaCell),
      market: cellCenter(...SPECIAL.marketCell),
      boss: cellCenter(...SPECIAL.bossCell),
      lockup: { x: -125, z: -16.5 }
    };
    return this;
  }

  layoutBlock(c) {
    const r = Math.random();
    const place = (x, z, w, d) => {
      const hr = Math.random();
      let bucket, h;
      if (hr < 0.33) { bucket = 0; h = rand(9, 16); }
      else if (hr < 0.72) { bucket = 1; h = rand(18, 40); }
      else { bucket = 2; h = rand(44, 84); if (w > 18) w = rand(12, 18); if (d > 18) d = rand(12, 18); }
      const spec = { x, z, w, d, h, bucket, tint: (Math.random() * FACADES.length) | 0 };
      this.bldSpecs.push(spec);
      this.boxCollider(x, z, w, d, h);
      if (Math.random() < 0.42) {
        this.stripSpecs.push({ x, z, w: w + 0.3, d: d + 0.3, y: h, color: pick(NEON) });
      }
      if (h > 50) this.stripSpecs.beacons = null;
      const faces = [
        { x: x, z: z - d / 2 - 0.12, ry: 0, fw: w },
        { x: x, z: z + d / 2 + 0.12, ry: Math.PI, fw: w },
        { x: x - w / 2 - 0.12, z: z, ry: Math.PI / 2, fw: d },
        { x: x + w / 2 + 0.12, z: z, ry: -Math.PI / 2, fw: d }
      ];
      const nSigns = Math.random() < 0.75 ? (Math.random() < 0.3 ? 2 : 1) : 0;
      for (let i = 0; i < nSigns; i++) {
        const f = pick(faces);
        const vertical = Math.random() < 0.22 && h > 14;
        if (vertical) {
          const sw = rand(1.7, 2.4), sh = Math.min(rand(7, 13), h - 4);
          this.signSpecs.push({ x: f.x, z: f.z, y: 4 + sh / 2, ry: f.ry, w: sw, h: sh, tex: (Math.random() * 22) | 0, vertical: true });
        } else {
          const sw = Math.min(rand(0.38, 0.72) * f.fw, rand(3, 11));
          const sh = sw * rand(0.3, 0.42);
          const y = rand(4, Math.max(5, h - 3));
          this.signSpecs.push({ x: f.x, z: f.z, y, ry: f.ry, w: sw, h: sh, tex: (Math.random() * 22) | 0, vertical: false });
        }
      }
    };

    if (r < 0.3) {
      const w = rand(16, 26), d = rand(16, 26);
      place(c.x + rand(-2, 2), c.z + rand(-2, 2), w, d);
    } else if (r < 0.7) {
      const along = Math.random() < 0.5;
      for (const s of [-1, 1]) {
        const w = rand(9, 12), d = rand(19, 26);
        if (along) place(c.x + s * 6.5 + rand(-1, 1), c.z + rand(-2, 2), w, d);
        else place(c.x + rand(-2, 2), c.z + s * 6.5 + rand(-1, 1), d, w);
      }
    } else {
      for (const sx of [-1, 1]) for (const sz of [-1, 1]) {
        place(c.x + sx * rand(5.5, 7), c.z + sz * rand(5.5, 7), rand(8, 11), rand(8, 11));
      }
      if (Math.random() < 0.3) this.pylonSpecs.push({ x: c.x, z: c.z, tex: (Math.random() * 22) | 0 });
    }
  }

  layoutPlaza(c) {
    this.pylonSpecs.push({ x: c.x, z: c.z, tex: 4, big: true });
    for (let i = 0; i < 6; i++) {
      const a = i / 6 * Math.PI * 2;
      const x = c.x + Math.cos(a) * 9, z = c.z + Math.sin(a) * 9;
      this.decorSpecs.push({ kind: 'bench', x, z, ry: -a });
      this.boxCollider(x, z, 1.9, 0.6, 0.75, false);
    }
    for (let i = 0; i < 3; i++) {
      const a = i / 3 * Math.PI * 2 + 0.5;
      const x = c.x + Math.cos(a) * 5, z = c.z + Math.sin(a) * 5;
      this.decorSpecs.push({ kind: 'planter', x, z, ry: a });
      this.boxCollider(x, z, 1.3, 1.3, 0.95, false);
    }
    this.signSpecs.push({ x: c.x, z: c.z - 13, y: 6, ry: 0, w: 10, h: 3, tex: 'KANEDA PLAZA', color: '#3ef2ff', vertical: false });
  }

  layoutLockup(c) {
    const w = 20, d = 14, h = 9;
    this.bldSpecs.push({ x: c.x, z: c.z, w, d, h, bucket: 0, tint: 3 });
    this.boxCollider(c.x, c.z, w, d, h);
    this.signSpecs.push({ x: c.x, z: c.z - d / 2 - 0.12, y: 6.5, ry: 0, w: 12, h: 2.4, tex: 'HELIX LOGISTICS', color: '#ffd60a', vertical: false });
    this.decorSpecs.push({ kind: 'door', x: c.x, z: c.z + d / 2 + 0.06 });
    for (const s of [-1, 1]) {
      const x = c.x + s * 8.5, z = c.z + 4;
      this.decorSpecs.push({ kind: 'container', x, z, ry: s * 0.08 });
      this.boxCollider(x, z, 6.2, 2.5, 2.5, true);
    }
  }

  layoutMarket(c) {
    for (let i = 0; i < 6; i++) {
      const x = c.x + (i % 3 - 1) * 9, z = c.z + ((i / 3) | 0 ? 7 : -7);
      this.decorSpecs.push({ kind: 'stall', x, z, ry: rand(-0.2, 0.2), color: pick(NEON) });
    }
    for (let i = 0; i < 8; i++) {
      const x = c.x + rand(-12, 12), z = c.z + rand(-11, 11);
      this.decorSpecs.push({ kind: 'crate', x, z, ry: rand(0, 3) });
      this.boxCollider(x, z, 1.25, 1.25, 1.2, false);
    }
    this.signSpecs.push({ x: c.x, z: c.z - 13, y: 5, ry: 0, w: 11, h: 2.6, tex: 'MOTO MARKET', color: '#ff2a6d', vertical: false });
  }

  layoutBoss(c) {
    for (let i = 0; i < 6; i++) {
      const a = i / 6 * Math.PI * 2 + 0.3;
      const x = c.x + Math.cos(a) * 11, z = c.z + Math.sin(a) * 11;
      this.decorSpecs.push({ kind: 'barricade', x, z, ry: -a + Math.PI / 2 });
      this.boxCollider(x, z, 2.4, 0.6, 1.15, false);
    }
    for (let i = 0; i < 5; i++) {
      const x = c.x + rand(-9, 9), z = c.z + rand(-9, 9);
      this.decorSpecs.push({ kind: 'crate', x, z, ry: rand(0, 3) });
      this.boxCollider(x, z, 1.25, 1.25, 1.2, false);
    }
    this.decorSpecs.push({ kind: 'rsSign', x: c.x, z: c.z - 12 });
    this.signSpecs.push({ x: c.x, z: c.z - 12.2, y: 7, ry: 0, w: 7, h: 3.4, tex: 'RS-7', color: '#ff2a6d', vertical: false });
  }

  layoutSpawnBlock(c) {
    placeSmall: {
      const x = c.x + 5, z = c.z - 4;
      this.bldSpecs.push({ x, z, w: 12, d: 12, h: 14, bucket: 0, tint: 0 });
      this.boxCollider(x, z, 12, 12, 14);
    }
    const kx = c.x + 6.5, kz = c.z + 6;
    this.kioskSpecs.push({ x: kx, z: kz, ry: -Math.PI / 2, tex: 'RIPPERDOC', color: '#ff2a6d' });
    this.boxCollider(kx, kz, 2.5, 3.4, 2.8, true);
    this.ripperdoc = { x: kx - 3.2, z: kz };
    this.decorSpecs.push({ kind: 'terminal', x: this.ripperdoc.x, z: this.ripperdoc.z, ry: -Math.PI / 2 });
    this.boxCollider(this.ripperdoc.x, this.ripperdoc.z, 0.9, 0.6, 1.15, false);
  }

  makePeds(c) {
    const n = Math.random() < 0.5 ? 0 : 1 + (Math.random() * 2 | 0);
    const out = [];
    for (let i = 0; i < n; i++) {
      const ring = 15.5;
      const side = (Math.random() * 4) | 0;
      const t = rand(-ring, ring);
      const x = side === 0 ? c.x + t : side === 1 ? c.x + t : side === 2 ? c.x - ring : c.x + ring;
      const z = side === 0 ? c.z - ring : side === 1 ? c.z + ring : c.z + t;
      out.push({ x, z, ring: { cx: c.x, cz: c.z, r: ring }, speed: rand(1.2, 1.9), tone: pick(['#233043', '#33202a', '#20302a', '#2a2a34', '#3a2416']), accent: pick(NEON) });
    }
    return out;
  }

  layoutStreets() {
    const lines = [-150, -100, -50, 0, 50, 100, 150];

    for (const L of lines) {
      for (let t = -190; t < 190; t += 30) {
        if (Math.abs(((t % 50) + 50) % 50 - 25) > 16) continue;
        for (const s of [-1, 1]) {
          if (Math.random() < 0.26) {
            const z = t + rand(-4, 4);
            this.carSpecs.push({ x: L + s * 6.7, z, ry: Math.PI / 2 + rand(-0.06, 0.06), color: pick(['#1a1f2b', '#232733', '#2b1f22', '#1c2a26', '#301a1e']) });
          }
          if (Math.random() < 0.26) {
            const x = t + rand(-4, 4);
            this.carSpecs.push({ x, z: L + s * 6.7, ry: rand(-0.06, 0.06), color: pick(['#1a1f2b', '#232733', '#2b1f22', '#1c2a26', '#301a1e']) });
          }
        }
      }
    }
    for (const car of this.carSpecs) this.boxCollider(car.x, car.z, 2, 4.4, 1.35, false);

    for (const L of lines) {
      for (let t = -196; t < 196; t += 4.5) {
        const m = ((t % 50) + 50) % 50;
        if (m > 9 && m < 41) {
          this.dashSpecs.push({ x: L, z: t, ry: 0 });
          this.dashSpecs.push({ x: t, z: L, ry: Math.PI / 2 });
        }
      }
    }

    for (const Lx of lines) for (const Lz of lines) {
      this.lampPos.push({ x: Lx + 8.5, z: Lz + 8.5 });
    }
    for (let i = 0; i < 9; i++) {
      const L = pick(lines);
      const t = rand(-160, 160);
      const horiz = Math.random() < 0.5;
      this.kioskSpecs.push({
        x: horiz ? t : L + 9.5, z: horiz ? L + 9.5 : t, ry: horiz ? 0 : Math.PI / 2,
        tex: pick(SIGN_WORDS), color: pick(NEON)
      });
    }
    for (const k of this.kioskSpecs) this.boxCollider(k.x, k.z, 3.2, 2.4, 2.7, true);

    const inter = [];
    for (const Lx of [-100, 0, 100]) for (const Lz of [-100, 0, 100]) inter.push({ x: Lx, z: Lz });
    for (let i = 0; i < 6; i++) {
      const p = inter.splice((Math.random() * inter.length) | 0, 1)[0];
      this.pylonSpecs.push({ x: p.x + rand(-3, 3), z: p.z + rand(-3, 3), tex: (Math.random() * 22) | 0 });
    }

    for (let i = 0; i < 7; i++) {
      const dir = rand(0, Math.PI * 2);
      this.flyerSpecs.push({ x: rand(-200, 200), z: rand(-200, 200), y: rand(38, 66), dir, speed: rand(9, 16) });
    }

    const arch = { x: 0, z: 150 };
    for (const s of [-1, 1]) this.boxCollider(arch.x + s * 7, arch.z, 1.2, 1.2, 9);
    this.decorSpecs.push({ kind: 'arch', x: arch.x, z: arch.z });
    this.pois.arch = arch;
  }

  layoutPerimeter() {
    for (let t = -180; t <= 180; t += 36) {
      for (const [x, z, rot] of [[t, 215, 0], [t, -215, 0], [215, t, 1], [-215, t, 1]]) {
        const w = rot ? rand(28, 34) : rand(24, 32), d = rot ? rand(24, 32) : rand(28, 34);
        const h = rand(55, 110);
        this.bldSpecs.push({ x, z, w, d, h, bucket: 2, tint: (Math.random() * FACADES.length) | 0, perimeter: true });
        this.addCollider(x - w / 2, x + w / 2, z - d / 2, z + d / 2, h);
      }
    }
  }

  /* ---------------- visuals ---------------- */

  canvas(w, h) {
    const c = document.createElement('canvas');
    c.width = w; c.height = h;
    return c;
  }

  makeWindowTexture(bucket, tint) {
    const rows = [8, 14, 22][bucket];
    const cols = 12;
    const c = this.canvas(256, 512);
    const g = c.getContext('2d');
    g.fillStyle = FACADES[tint];
    g.fillRect(0, 0, 256, 512);
    g.fillStyle = 'rgba(255,255,255,.02)';
    for (let y = 0; y < 512; y += 32) g.fillRect(0, y, 256, 1);
    for (let i = 0; i < 26; i++) {
      const x = Math.random() * 256, w = rand(3, 12);
      const gl = g.createLinearGradient(0, 0, 0, 512);
      gl.addColorStop(0, 'rgba(0,0,0,0)');
      gl.addColorStop(0.3, `rgba(8,10,14,${rand(0.05, 0.14)})`);
      gl.addColorStop(1, 'rgba(0,0,0,0)');
      g.fillStyle = gl;
      g.fillRect(x, 0, w, 512);
    }
    const cw = 256 / cols, ch = 512 / rows;
    const lit = ['#ffd9a0', '#9fd8ff', '#ff9ad5', '#cfffec', '#ffe8c8'];
    for (let r = 0; r < rows; r++) {
      g.fillStyle = 'rgba(0,0,0,.22)';
      g.fillRect(0, r * ch, 256, 2);
      for (let q = 0; q < cols; q++) {
        const wide = Math.random() < 0.08 && q < cols - 1;
        const x = q * cw + 3, y = r * ch + 3.5;
        const w = cw * (wide ? 2 : 1) - 6, h = ch - 7;
        if (Math.random() < 0.34) {
          g.globalAlpha = rand(0.35, 1);
          g.fillStyle = pick(lit);
        } else {
          g.globalAlpha = 1;
          g.fillStyle = Math.random() < 0.5 ? '#05070c' : '#0a0e16';
        }
        g.fillRect(x, y, w, h);
        if (wide) q++;
      }
    }
    if (bucket === 0) {
      const y0 = 512 - 46;
      g.globalAlpha = 1;
      g.fillStyle = 'rgba(20,26,40,.9)';
      g.fillRect(0, y0, 256, 46);
      for (let i = 0; i < 9; i++) {
        g.globalAlpha = rand(0.5, 1);
        g.fillStyle = pick(NEON);
        g.fillRect(6 + i * 28, y0 + 6, 20, 30);
      }
    }
    g.globalAlpha = 1;
    const t = new THREE.CanvasTexture(c);
    t.colorSpace = THREE.SRGBColorSpace;
    t.anisotropy = 4;
    return t;
  }

  makeSignTexture(text, color, vertical) {
    const c = vertical ? this.canvas(128, 384) : this.canvas(256, 96);
    const g = c.getContext('2d');
    g.fillStyle = 'rgba(7,9,15,.95)';
    g.fillRect(0, 0, c.width, c.height);
    g.strokeStyle = color;
    g.lineWidth = 5;
    g.strokeRect(4, 4, c.width - 8, c.height - 8);
    g.shadowColor = color;
    g.shadowBlur = 20;
    g.fillStyle = color;
    if (vertical) {
      g.font = 'bold 58px Arial';
      g.textAlign = 'center';
      g.textBaseline = 'middle';
      const chars = text.replace(/[a-z ]/g, '').slice(0, 6) || text.slice(0, 5).toUpperCase();
      const step = c.height / (chars.length + 1);
      for (let i = 0; i < chars.length; i++) g.fillText(chars[i], c.width / 2, step * (i + 1));
    } else {
      const size = text.length > 9 ? 46 : 60;
      g.font = `bold ${size}px Arial`;
      g.textAlign = 'center';
      g.textBaseline = 'middle';
      g.fillText(text, c.width / 2, c.height / 2 + 2);
    }
    const t = new THREE.CanvasTexture(c);
    t.colorSpace = THREE.SRGBColorSpace;
    return t;
  }

  buildVisuals() {
    if (this.built) return;
    this.built = true;
    const scene = this.game.scene;

    scene.fog = new THREE.FogExp2(0x0a0c18, 0.0052);
    scene.background = new THREE.Color(0x070a14);

    const skyC = this.canvas(1024, 512);
    const sg = skyC.getContext('2d');
    const grd = sg.createLinearGradient(0, 0, 0, 512);
    grd.addColorStop(0, '#02040c');
    grd.addColorStop(0.4, '#0a1130');
    grd.addColorStop(0.62, '#1c1440');
    grd.addColorStop(0.72, '#43204d');
    grd.addColorStop(0.78, '#83395a');
    grd.addColorStop(0.82, '#5a2a4d');
    grd.addColorStop(0.88, '#170f2c');
    grd.addColorStop(1, '#0b0818');
    sg.fillStyle = grd;
    sg.fillRect(0, 0, 1024, 512);
    for (let i = 0; i < 16; i++) {
      const cx = rand(0, 1024), cy = rand(90, 330), cw = rand(80, 280), chh = rand(8, 26);
      const lit = Math.random() < 0.35;
      sg.fillStyle = lit ? 'rgba(120,60,110,0.35)' : 'rgba(16,12,38,0.55)';
      sg.beginPath();
      sg.ellipse(cx, cy, cw, chh, 0, 0, Math.PI * 2);
      sg.fill();
      if (lit) {
        sg.fillStyle = 'rgba(255,110,150,0.18)';
        sg.beginPath();
        sg.ellipse(cx, cy + chh * 0.35, cw * 0.85, chh * 0.5, 0, 0, Math.PI * 2);
        sg.fill();
      }
    }
    for (let i = 0; i < 160; i++) {
      sg.fillStyle = `rgba(255,255,255,${rand(0.15, 0.9)})`;
      sg.fillRect(Math.random() * 1024, Math.random() * 200, rand(0.6, 1.8), rand(0.6, 1.8));
    }
    const mx = 740, my = 110;
    const mglow = sg.createRadialGradient(mx, my, 0, mx, my, 70);
    mglow.addColorStop(0, 'rgba(190,205,255,.5)');
    mglow.addColorStop(1, 'rgba(190,205,255,0)');
    sg.fillStyle = mglow;
    sg.fillRect(mx - 70, my - 70, 140, 140);
    sg.fillStyle = '#dfe8ff';
    sg.beginPath();
    sg.arc(mx, my, 17, 0, Math.PI * 2);
    sg.fill();
    sg.fillStyle = 'rgba(140,150,190,.5)';
    sg.beginPath();
    sg.arc(mx - 5, my - 4, 4, 0, Math.PI * 2);
    sg.arc(mx + 6, my + 5, 3, 0, Math.PI * 2);
    sg.fill();
    const skyTex = new THREE.CanvasTexture(skyC);
    skyTex.colorSpace = THREE.SRGBColorSpace;
    const sky = new THREE.Mesh(
      new THREE.SphereGeometry(390, 32, 20),
      new THREE.MeshBasicMaterial({ map: skyTex, side: THREE.BackSide, fog: false })
    );
    scene.add(sky);

    const envScene = new THREE.Scene();
    envScene.add(new THREE.Mesh(
      new THREE.SphereGeometry(60, 16, 12),
      new THREE.MeshBasicMaterial({ map: skyTex, side: THREE.BackSide })
    ));
    const envFloor = new THREE.Mesh(
      new THREE.PlaneGeometry(130, 130),
      new THREE.MeshBasicMaterial({ color: 0x0a0d16 })
    );
    envFloor.rotation.x = -Math.PI / 2;
    envFloor.position.y = -4;
    envScene.add(envFloor);
    const envBlob = new THREE.SphereGeometry(1, 8, 6);
    for (let i = 0; i < 26; i++) {
      const m = new THREE.Mesh(envBlob, new THREE.MeshBasicMaterial({ color: pick(NEON), toneMapped: false }));
      const a = rand(0, Math.PI * 2), r = rand(14, 44);
      m.position.set(Math.cos(a) * r, rand(-1, 15), Math.sin(a) * r);
      m.scale.set(rand(2, 6), rand(1, 4), rand(2, 6));
      envScene.add(m);
    }
    const pmrem = new THREE.PMREMGenerator(this.game.renderer);
    this.envRT = pmrem.fromScene(envScene, 0.05);
    scene.environment = this.envRT.texture;
    scene.environmentIntensity = 0.55;
    pmrem.dispose();

    const asC = this.canvas(512, 512);
    const ag = asC.getContext('2d');
    ag.fillStyle = '#0d1018';
    ag.fillRect(0, 0, 512, 512);
    for (let i = 0; i < 2600; i++) {
      ag.fillStyle = Math.random() < 0.5 ? 'rgba(255,255,255,.03)' : 'rgba(0,0,0,.16)';
      ag.fillRect(Math.random() * 512, Math.random() * 512, rand(1, 2.4), rand(1, 2.4));
    }
    for (let i = 0; i < 26; i++) {
      ag.strokeStyle = 'rgba(0,0,0,.28)';
      ag.lineWidth = rand(0.6, 1.6);
      ag.beginPath();
      const x0 = Math.random() * 512, y0 = Math.random() * 512;
      ag.moveTo(x0, y0);
      ag.lineTo(x0 + rand(-60, 60), y0 + rand(-60, 60));
      ag.stroke();
    }
    const asTex = new THREE.CanvasTexture(asC);
    asTex.colorSpace = THREE.SRGBColorSpace;
    asTex.wrapS = asTex.wrapT = THREE.RepeatWrapping;
    asTex.repeat.set(14, 14);
    const roughC = this.canvas(512, 512);
    const rgc = roughC.getContext('2d');
    rgc.fillStyle = '#6f6f6f';
    rgc.fillRect(0, 0, 512, 512);
    for (let i = 0; i < 24; i++) {
      const x = Math.random() * 512, y = Math.random() * 512, r = rand(18, 60);
      const pg = rgc.createRadialGradient(x, y, 0, x, y, r);
      pg.addColorStop(0, 'rgba(30,30,30,.95)');
      pg.addColorStop(0.7, 'rgba(50,50,50,.6)');
      pg.addColorStop(1, 'rgba(110,110,110,0)');
      rgc.fillStyle = pg;
      rgc.beginPath();
      rgc.ellipse(x, y, r, r * rand(0.5, 0.9), rand(0, 3), 0, Math.PI * 2);
      rgc.fill();
    }
    const roughTex = new THREE.CanvasTexture(roughC);
    roughTex.wrapS = roughTex.wrapT = THREE.RepeatWrapping;
    roughTex.repeat.set(14, 14);
    const ground = new THREE.Mesh(
      new THREE.PlaneGeometry(470, 470),
      new THREE.MeshStandardMaterial({
        map: asTex, roughnessMap: roughTex, roughness: 1, metalness: 0.72, envMapIntensity: 1.25
      })
    );
    ground.rotation.x = -Math.PI / 2;
    scene.add(ground);

    scene.add(new THREE.HemisphereLight(0x333a6a, 0x0b0a12, 0.55));
    const dir = new THREE.DirectionalLight(0x8890ff, 0.5);
    dir.position.set(60, 120, 40);
    scene.add(dir);
    const pl = [
      [0xff2a6d, 75, 7, 25, 70], [0x3ef2ff, 25, 7, -75, 60], [0xffd60a, 8, 6, 140, 55], [0xff3344, 125, 9, -125, 80]
    ];
    for (const [c, x, y, z, i] of pl) {
      const l = new THREE.PointLight(c, i, 75, 2);
      l.position.set(x, y, z);
      scene.add(l);
    }

    // buildings
    const roofMat = new THREE.MeshStandardMaterial({ color: 0x0a0d13, roughness: 0.95 });
    const wallMats = [0, 1, 2].map(b => FACADES.map((f, ti) => {
      const tex = this.makeWindowTexture(b, ti);
      return new THREE.MeshStandardMaterial({
        map: tex, emissive: 0xffffff, emissiveMap: tex, emissiveIntensity: 1.15,
        roughness: 0.92, metalness: 0.08
      });
    }));
    const stripInst = [];
    for (const b of this.bldSpecs) {
      const geo = new THREE.BoxGeometry(b.w, b.h, b.d);
      const wall = wallMats[b.bucket][b.tint];
      const mesh = new THREE.Mesh(geo, [wall, wall, roofMat, roofMat, wall, wall]);
      mesh.position.set(b.x, b.h / 2, b.z);
      scene.add(mesh);
      if (!b.perimeter && Math.random() < 0.55) {
        stripInst.push({ b, face: (Math.random() * 4) | 0 });
      }
      if (b.h > 50 && !b.perimeter) {
        const pole = new THREE.Mesh(
          new THREE.CylinderGeometry(0.06, 0.06, 3, 5),
          new THREE.MeshBasicMaterial({ color: 0x1a2030 })
        );
        pole.position.set(b.x, b.h + 1.5, b.z);
        scene.add(pole);
        const bcn = new THREE.Mesh(
          new THREE.SphereGeometry(0.22, 8, 6),
          new THREE.MeshBasicMaterial({ color: 0xff2233, toneMapped: false })
        );
        bcn.position.set(b.x, b.h + 3, b.z);
        scene.add(bcn);
        (this.beacons ||= []).push(bcn);
      }
    }

    if (stripInst.length) {
      const sGeo = new THREE.BoxGeometry(1, 0.5, 0.08);
      const sMat = new THREE.MeshBasicMaterial({ toneMapped: false });
      const sIM = new THREE.InstancedMesh(sGeo, sMat, stripInst.length);
      const sdummy = new THREE.Object3D();
      const scol = new THREE.Color();
      stripInst.forEach((s, i) => {
        const b = s.b;
        let x, z, ry, len;
        if (s.face === 0) { x = b.x; z = b.z - b.d / 2 - 0.07; ry = 0; len = b.w * rand(0.5, 0.8); }
        else if (s.face === 1) { x = b.x; z = b.z + b.d / 2 + 0.07; ry = Math.PI; len = b.w * rand(0.5, 0.8); }
        else if (s.face === 2) { x = b.x - b.w / 2 - 0.07; z = b.z; ry = Math.PI / 2; len = b.d * rand(0.5, 0.8); }
        else { x = b.x + b.w / 2 + 0.07; z = b.z; ry = -Math.PI / 2; len = b.d * rand(0.5, 0.8); }
        sdummy.position.set(x, 3.4, z);
        sdummy.rotation.set(0, ry, 0);
        sdummy.scale.set(len, 1, 1);
        sdummy.updateMatrix();
        sIM.setMatrixAt(i, sdummy.matrix);
        sIM.setColorAt(i, scol.set(pick(NEON)).multiplyScalar(rand(0.55, 1)));
      });
      sIM.instanceMatrix.needsUpdate = true;
      if (sIM.instanceColor) sIM.instanceColor.needsUpdate = true;
      scene.add(sIM);
    }

    // roof trim strips
    for (const s of this.stripSpecs) {
      const m = new THREE.Mesh(
        new THREE.BoxGeometry(s.w, 0.22, s.d),
        new THREE.MeshBasicMaterial({ color: new THREE.Color(s.color), toneMapped: false })
      );
      m.position.set(s.x, s.y, s.z);
      scene.add(m);
    }

    // signs
    const signTexes = [];
    for (let i = 0; i < 22; i++) {
      signTexes.push(this.makeSignTexture(pick(SIGN_WORDS), pick(NEON), false));
    }
    this.flickers = [];
    for (const s of this.signSpecs) {
      let tex;
      if (typeof s.tex === 'string') tex = this.makeSignTexture(s.tex, s.color || '#3ef2ff', s.vertical);
      else tex = signTexes[s.tex % signTexes.length];
      const mat = new THREE.MeshBasicMaterial({ map: tex, toneMapped: false });
      const mesh = new THREE.Mesh(new THREE.PlaneGeometry(s.w, s.h), mat);
      mesh.position.set(s.x, s.y, s.z);
      mesh.rotation.y = s.ry;
      scene.add(mesh);
      const back = new THREE.Mesh(
        new THREE.BoxGeometry(s.vertical ? s.w + 0.2 : s.w + 0.3, s.h + 0.2, 0.12),
        new THREE.MeshStandardMaterial({ color: 0x11141c, roughness: 0.9 })
      );
      back.position.copy(mesh.position).addScaledVector(
        new THREE.Vector3(Math.sin(s.ry), 0, Math.cos(s.ry)), -0.09
      );
      back.rotation.y = s.ry;
      scene.add(back);
      if (Math.random() < 0.3) this.flickers.push({ mat, seed: rand(0, 100) });
    }

    // perpendicular hanging neon signs
    let hAdded = 0;
    for (let i = 0; i < 90 && hAdded < 26; i++) {
      const s = this.signSpecs[(Math.random() * this.signSpecs.length) | 0];
      if (s.y > 13) continue;
      const tex = signTexes[(Math.random() * signTexes.length) | 0];
      const w = Math.min(s.w * 0.9, 4.2), h = w * 0.38;
      const out = 0.85 + w / 2;
      const nx = Math.sin(s.ry), nz = Math.cos(s.ry);
      const m = new THREE.Mesh(
        new THREE.PlaneGeometry(w, h),
        new THREE.MeshBasicMaterial({ map: tex, toneMapped: false, side: THREE.DoubleSide })
      );
      m.position.set(s.x + nx * out, s.y, s.z + nz * out);
      m.rotation.y = s.ry + Math.PI / 2;
      scene.add(m);
      const bar = new THREE.Mesh(
        new THREE.BoxGeometry(0.06, 0.06, out),
        new THREE.MeshStandardMaterial({ color: 0x11141c })
      );
      bar.position.set(s.x + nx * out / 2, s.y + h / 2 + 0.06, s.z + nz * out / 2);
      bar.rotation.y = s.ry;
      scene.add(bar);
      hAdded++;
    }

    // animated billboards on tall buildings
    this.billboards = [];
    const talls = this.bldSpecs.filter(b => b.h > 42 && !b.perimeter);
    const bWords = ['SEKTOR 7', 'ネオン', 'HELIX', 'VOLT', '未来', 'RAMEN', '入電', 'GHOST'];
    for (let i = 0; i < Math.min(4, talls.length); i++) {
      const b = talls[(Math.random() * talls.length) | 0];
      const c = this.canvas(256, 1024);
      const g2 = c.getContext('2d');
      for (let band = 0; band < 8; band++) {
        const y = band * 128;
        g2.fillStyle = pick(['#0a0212', '#12021a', '#021018']);
        g2.fillRect(0, y, 256, 128);
        g2.fillStyle = pick(NEON);
        g2.shadowColor = g2.fillStyle;
        g2.shadowBlur = 18;
        g2.font = 'bold 58px Arial';
        g2.textAlign = 'center';
        g2.textBaseline = 'middle';
        g2.fillText(pick(bWords), 128, y + 60);
        g2.shadowBlur = 0;
        g2.fillStyle = 'rgba(255,255,255,.08)';
        g2.fillRect(0, y + 120, 256, 8);
      }
      const tex = new THREE.CanvasTexture(c);
      tex.colorSpace = THREE.SRGBColorSpace;
      tex.wrapT = THREE.RepeatWrapping;
      const m = new THREE.Mesh(
        new THREE.PlaneGeometry(7, 9.8),
        new THREE.MeshBasicMaterial({ map: tex, toneMapped: false, side: THREE.DoubleSide })
      );
      m.position.set(b.x, b.h * 0.55, b.z + b.d / 2 + 0.18);
      scene.add(m);
      this.billboards.push(tex);
    }

    // parked cars
    const carGeo = new THREE.BoxGeometry(1.9, 1.05, 4.3);
    const cabGeo = new THREE.BoxGeometry(1.7, 0.75, 2.2);
    const tailGeo = new THREE.BoxGeometry(1.7, 0.12, 0.08);
    for (const car of this.carSpecs) {
      const g = new THREE.Group();
      const body = new THREE.Mesh(carGeo, new THREE.MeshStandardMaterial({ color: car.color, roughness: 0.4, metalness: 0.7 }));
      body.position.y = 0.55;
      const cab = new THREE.Mesh(cabGeo, new THREE.MeshStandardMaterial({ color: 0x0c1018, roughness: 0.25, metalness: 0.8 }));
      cab.position.set(0, 1.25, -0.3);
      g.add(body, cab);
      for (const zz of [-2.16, 2.16]) {
        const tail = new THREE.Mesh(tailGeo, new THREE.MeshBasicMaterial({
          color: zz < 0 ? 0xff2222 : 0xeef6ff, toneMapped: false
        }));
        tail.position.set(0, 0.72, zz);
        g.add(tail);
      }
      g.position.set(car.x, 0, car.z);
      g.rotation.y = car.ry;
      scene.add(g);
    }

    // street lamps (instanced)
    const poleIM = new THREE.InstancedMesh(
      new THREE.CylinderGeometry(0.07, 0.1, 5.6, 6),
      new THREE.MeshStandardMaterial({ color: 0x1c222e, roughness: 0.7 }),
      this.lampPos.length
    );
    const tubeIM = new THREE.InstancedMesh(
      new THREE.BoxGeometry(1.5, 0.1, 0.12),
      new THREE.MeshBasicMaterial({ color: 0xbfefff, toneMapped: false }),
      this.lampPos.length
    );
    const dummy = new THREE.Object3D();
    this.lampPos.forEach((p, i) => {
      dummy.position.set(p.x, 2.8, p.z);
      dummy.rotation.set(0, 0, 0);
      dummy.updateMatrix();
      poleIM.setMatrixAt(i, dummy.matrix);
      dummy.position.set(p.x, 5.62, p.z);
      dummy.rotation.y = Math.PI / 4;
      dummy.updateMatrix();
      tubeIM.setMatrixAt(i, dummy.matrix);
    });
    scene.add(poleIM, tubeIM);

    const coneIM = new THREE.InstancedMesh(
      new THREE.ConeGeometry(1.35, 5.4, 8, 1, true),
      new THREE.MeshBasicMaterial({
        color: 0x86c5e8, transparent: true, opacity: 0.055,
        blending: THREE.AdditiveBlending, depthWrite: false, side: THREE.DoubleSide
      }),
      this.lampPos.length
    );
    this.lampPos.forEach((p, i) => {
      dummy.position.set(p.x, 2.9, p.z);
      dummy.rotation.set(0, 0, 0);
      dummy.scale.set(1, 1, 1);
      dummy.updateMatrix();
      coneIM.setMatrixAt(i, dummy.matrix);
    });
    scene.add(coneIM);

    // lane dashes (instanced)
    const dashIM = new THREE.InstancedMesh(
      new THREE.BoxGeometry(0.16, 0.02, 1.9),
      new THREE.MeshBasicMaterial({ color: 0x6b5a2e, toneMapped: false }),
      this.dashSpecs.length
    );
    this.dashSpecs.forEach((d, i) => {
      dummy.position.set(d.x, 0.02, d.z);
      dummy.rotation.set(0, d.ry, 0);
      dummy.updateMatrix();
      dashIM.setMatrixAt(i, dummy.matrix);
    });
    scene.add(dashIM);

    // holo pylons
    this.pylonMeshes = [];
    for (const p of this.pylonSpecs) {
      const g = new THREE.Group();
      const pole = new THREE.Mesh(
        new THREE.CylinderGeometry(0.12, 0.16, p.big ? 8 : 6, 6),
        new THREE.MeshStandardMaterial({ color: 0x1c222e, roughness: 0.7 })
      );
      pole.position.y = p.big ? 4 : 3;
      g.add(pole);
      const tex = typeof p.tex === 'number'
        ? signTexes[p.tex % signTexes.length]
        : this.makeSignTexture(String(p.tex), '#3ef2ff', false);
      const size = p.big ? 2 : 1;
      const adMat = new THREE.MeshBasicMaterial({ map: tex, toneMapped: false, transparent: true, opacity: 0.92, side: THREE.DoubleSide });
      for (const ry of [0, Math.PI / 2]) {
        const ad = new THREE.Mesh(new THREE.PlaneGeometry(3 * size, 4.4 * size), adMat);
        ad.position.y = p.big ? 6.6 : 5.2;
        ad.rotation.y = ry;
        g.add(ad);
      }
      g.position.set(p.x, 0, p.z);
      scene.add(g);
      this.pylonMeshes.push(g);
    }

    // kiosks
    for (const k of this.kioskSpecs) {
      const g = new THREE.Group();
      const body = new THREE.Mesh(
        new THREE.BoxGeometry(3, 2.4, 2),
        new THREE.MeshStandardMaterial({ color: 0x141a24, roughness: 0.8 })
      );
      body.position.y = 1.3;
      const signTex = this.makeSignTexture(k.tex, k.color, false);
      const sign = new THREE.Mesh(new THREE.PlaneGeometry(2.9, 1), new THREE.MeshBasicMaterial({ map: signTex, toneMapped: false }));
      sign.position.set(0, 2.85, 0);
      sign.rotation.y = Math.PI / 2;
      const glow = new THREE.Mesh(
        new THREE.BoxGeometry(0.08, 1.6, 1.8),
        new THREE.MeshBasicMaterial({ color: new THREE.Color(k.color), toneMapped: false })
      );
      glow.position.set(1.55, 1.3, 0);
      g.add(body, sign, glow);
      g.position.set(k.x, 0, k.z);
      g.rotation.y = k.ry;
      scene.add(g);
    }

    // décor props
    const benchMat = new THREE.MeshStandardMaterial({ color: 0x1a222e, roughness: 0.85 });
    for (const d of this.decorSpecs) {
      if (d.kind === 'bench') {
        const m = new THREE.Mesh(new THREE.BoxGeometry(1.9, 0.16, 0.55), benchMat);
        m.position.set(d.x, 0.62, d.z);
        m.rotation.y = d.ry;
        scene.add(m);
        for (const s of [-0.7, 0.7]) {
          const leg = new THREE.Mesh(new THREE.BoxGeometry(0.12, 0.6, 0.4), benchMat);
          leg.position.set(d.x + Math.cos(d.ry) * s, 0.3, d.z - Math.sin(d.ry) * s);
          scene.add(leg);
        }
      } else if (d.kind === 'planter') {
        const m = new THREE.Mesh(new THREE.BoxGeometry(1.3, 0.9, 1.3), benchMat);
        m.position.set(d.x, 0.45, d.z);
        const bush = new THREE.Mesh(
          new THREE.SphereGeometry(0.55, 7, 5),
          new THREE.MeshBasicMaterial({ color: 0x1f8f5f, toneMapped: false })
        );
        bush.position.set(d.x, 1.15, d.z);
        scene.add(m, bush);
      } else if (d.kind === 'door') {
        const m = new THREE.Mesh(
          new THREE.BoxGeometry(3.2, 4.2, 0.16),
          new THREE.MeshBasicMaterial({ color: 0x66e6ff, toneMapped: false, transparent: true, opacity: 0.35 })
        );
        m.position.set(d.x, 2.1, d.z);
        scene.add(m);
      } else if (d.kind === 'container') {
        const m = new THREE.Mesh(
          new THREE.BoxGeometry(6.2, 2.5, 2.5),
          new THREE.MeshStandardMaterial({ color: 0x27351f, roughness: 0.85 })
        );
        m.position.set(d.x, 1.25, d.z);
        m.rotation.y = d.ry;
        const strip = new THREE.Mesh(
          new THREE.BoxGeometry(6.24, 0.14, 2.54),
          new THREE.MeshBasicMaterial({ color: 0xffd60a, toneMapped: false })
        );
        strip.position.set(d.x, 2.2, d.z);
        strip.rotation.y = d.ry;
        scene.add(m, strip);
      } else if (d.kind === 'stall') {
        const canopy = new THREE.Mesh(
          new THREE.PlaneGeometry(3.4, 3),
          new THREE.MeshBasicMaterial({ color: new THREE.Color(d.color), toneMapped: false, transparent: true, opacity: 0.4, side: THREE.DoubleSide })
        );
        canopy.position.set(d.x, 2.3, d.z);
        canopy.rotation.set(-Math.PI / 2 + 0.15, d.ry, 0);
        scene.add(canopy);
        for (const s of [-1, 1]) {
          const pole = new THREE.Mesh(new THREE.CylinderGeometry(0.05, 0.05, 2.3, 5), benchMat);
          pole.position.set(d.x + Math.cos(d.ry) * s * 1.5, 1.15, d.z - Math.sin(d.ry) * s * 1.5);
          scene.add(pole);
        }
      } else if (d.kind === 'crate') {
        const m = new THREE.Mesh(
          new THREE.BoxGeometry(1.25, 1.2, 1.25),
          new THREE.MeshStandardMaterial({ color: 0x232a36, roughness: 0.9 })
        );
        m.position.set(d.x, 0.6, d.z);
        m.rotation.y = d.ry;
        scene.add(m);
      } else if (d.kind === 'barricade') {
        const m = new THREE.Mesh(
          new THREE.BoxGeometry(2.4, 1.1, 0.35),
          new THREE.MeshStandardMaterial({ color: 0x2a2026, roughness: 0.9 })
        );
        m.position.set(d.x, 0.55, d.z);
        m.rotation.y = d.ry;
        const stripe = new THREE.Mesh(
          new THREE.BoxGeometry(2.42, 0.18, 0.37),
          new THREE.MeshBasicMaterial({ color: 0xff2a6d, toneMapped: false })
        );
        stripe.position.set(d.x, 0.85, d.z);
        stripe.rotation.y = d.ry;
        scene.add(m, stripe);
      } else if (d.kind === 'rsSign') {
        const pole = new THREE.Mesh(new THREE.CylinderGeometry(0.14, 0.18, 7, 6), benchMat);
        pole.position.set(d.x, 3.5, d.z);
        scene.add(pole);
      } else if (d.kind === 'terminal') {
        const g = new THREE.Group();
        const base = new THREE.Mesh(new THREE.BoxGeometry(0.9, 0.5, 0.6), new THREE.MeshStandardMaterial({ color: 0x161c28, roughness: 0.6, metalness: 0.5 }));
        base.position.y = 0.85;
        const screen = new THREE.Mesh(
          new THREE.PlaneGeometry(0.8, 0.42),
          new THREE.MeshBasicMaterial({ color: 0x3ef2ff, toneMapped: false })
        );
        screen.position.set(0, 1.4, 0.12);
        screen.rotation.x = -0.35;
        const stand = new THREE.Mesh(new THREE.BoxGeometry(0.12, 0.9, 0.12), new THREE.MeshStandardMaterial({ color: 0x1c222e }));
        stand.position.y = 0.45;
        g.add(base, screen, stand);
        g.position.set(d.x, 0, d.z);
        g.rotation.y = d.ry;
        scene.add(g);
      } else if (d.kind === 'arch') {
        for (const s of [-1, 1]) {
          const p = new THREE.Mesh(
            new THREE.BoxGeometry(1.2, 9, 1.2),
            new THREE.MeshStandardMaterial({ color: 0x141a24, roughness: 0.8 })
          );
          p.position.set(d.x + s * 7, 4.5, d.z);
          scene.add(p);
        }
        const beam = new THREE.Mesh(
          new THREE.BoxGeometry(15.6, 1.5, 1),
          new THREE.MeshStandardMaterial({ color: 0x141a24, roughness: 0.8 })
        );
        beam.position.set(d.x, 9.3, d.z);
        scene.add(beam);
        const archTex = this.makeSignTexture('SEKTOR 7', '#3ef2ff', false);
        for (const sz of [-0.56, 0.56]) {
          const s = new THREE.Mesh(new THREE.PlaneGeometry(10, 1.1), new THREE.MeshBasicMaterial({ map: archTex, toneMapped: false }));
          s.position.set(d.x, 9.3, d.z + sz);
          if (sz < 0) s.rotation.y = Math.PI;
          scene.add(s);
        }
      }
    }

    // pedestrians
    this.peds = [];
    const bodyGeo = new THREE.CylinderGeometry(0.24, 0.3, 1.05, 7);
    const headGeo = new THREE.SphereGeometry(0.16, 8, 6);
    for (const p of this.pedSpecs) {
      const g = new THREE.Group();
      const body = new THREE.Mesh(bodyGeo, new THREE.MeshStandardMaterial({ color: p.tone, roughness: 0.9 }));
      body.position.y = 0.55;
      const head = new THREE.Mesh(headGeo, new THREE.MeshStandardMaterial({ color: 0x8a7a6d, roughness: 0.9 }));
      head.position.y = 1.22;
      const badge = new THREE.Mesh(
        new THREE.BoxGeometry(0.34, 0.06, 0.02),
        new THREE.MeshBasicMaterial({ color: new THREE.Color(p.accent), toneMapped: false })
      );
      badge.position.set(0, 0.9, 0.29);
      g.add(body, head, badge);
      g.position.set(p.x, 0, p.z);
      scene.add(g);
      this.peds.push({ g, spec: p, target: this.pedTarget(p), wait: 0, phase: rand(0, 9) });
    }

    // flying vehicles
    this.flyers = [];
    for (const f of this.flyerSpecs) {
      const g = new THREE.Group();
      const body = new THREE.Mesh(
        new THREE.BoxGeometry(3.4, 0.5, 1.5),
        new THREE.MeshStandardMaterial({ color: 0x11161f, roughness: 0.5, metalness: 0.7 })
      );
      const nose = new THREE.Mesh(
        new THREE.BoxGeometry(0.9, 0.16, 1.3),
        new THREE.MeshBasicMaterial({ color: 0xdff4ff, toneMapped: false })
      );
      nose.position.x = -1.9;
      const tailL = new THREE.Mesh(
        new THREE.BoxGeometry(0.08, 0.14, 1.2),
        new THREE.MeshBasicMaterial({ color: 0xff3344, toneMapped: false })
      );
      tailL.position.x = 1.74;
      const beacon = new THREE.Mesh(
        new THREE.SphereGeometry(0.09, 6, 5),
        new THREE.MeshBasicMaterial({ color: 0xffd60a, toneMapped: false })
      );
      beacon.position.set(0, 0.4, 0);
      g.add(body, nose, tailL, beacon);
      g.position.set(f.x, f.y, f.z);
      g.rotation.y = -f.dir;
      scene.add(g);
      this.flyers.push({ g, spec: f, beacon, ph: rand(0, 9) });
    }

    // rain
    const RN = 2400;
    const rainPos = new Float32Array(RN * 3);
    for (let i = 0; i < RN; i++) {
      rainPos[i * 3] = rand(-30, 30);
      rainPos[i * 3 + 1] = rand(0, 40);
      rainPos[i * 3 + 2] = rand(-30, 30);
    }
    const rainGeo = new THREE.BufferGeometry();
    rainGeo.setAttribute('position', new THREE.BufferAttribute(rainPos, 3));
    const rc = this.canvas(8, 32);
    const rg = rc.getContext('2d');
    const rgrd = rg.createLinearGradient(0, 0, 0, 32);
    rgrd.addColorStop(0, 'rgba(160,215,255,0)');
    rgrd.addColorStop(0.5, 'rgba(160,215,255,.85)');
    rgrd.addColorStop(1, 'rgba(160,215,255,0)');
    rg.fillStyle = rgrd;
    rg.fillRect(3, 0, 2, 32);
    const rainTex = new THREE.CanvasTexture(rc);
    rainTex.colorSpace = THREE.SRGBColorSpace;
    this.rain = new THREE.Points(rainGeo, new THREE.PointsMaterial({
      size: 0.6, map: rainTex, transparent: true, opacity: 0.5,
      depthWrite: false, color: 0x9fd8ff
    }));
    this.rain.frustumCulled = false;
    scene.add(this.rain);
    this.rainPos = rainPos;

    const bc = this.canvas(128, 128);
    const bg = bc.getContext('2d');
    const bgrd = bg.createRadialGradient(64, 64, 0, 64, 64, 64);
    bgrd.addColorStop(0, 'rgba(0,0,0,.75)');
    bgrd.addColorStop(0.55, 'rgba(0,0,0,.4)');
    bgrd.addColorStop(1, 'rgba(0,0,0,0)');
    bg.fillStyle = bgrd;
    bg.fillRect(0, 0, 128, 128);
    this.blobTex = new THREE.CanvasTexture(bc);
    this.time = 0;
  }

  makeBlob(scale = 1.4) {
    if (!this.blobTex) return null;
    const m = new THREE.Mesh(
      new THREE.PlaneGeometry(scale, scale),
      new THREE.MeshBasicMaterial({ map: this.blobTex, transparent: true, depthWrite: false })
    );
    m.rotation.x = -Math.PI / 2;
    m.position.y = 0.03;
    return m;
  }

  pedTarget(p) {
    const r = p.ring;
    const side = (Math.random() * 4) | 0;
    const t = rand(-r.r, r.r);
    if (side === 0) return { x: r.cx + t, z: r.cz - r.r };
    if (side === 1) return { x: r.cx + t, z: r.cz + r.r };
    if (side === 2) return { x: r.cx - r.r, z: r.cz + t };
    return { x: r.cx + r.r, z: r.cz + t };
  }

  update(dt) {
    this.time = (this.time || 0) + dt;
    const t = this.time;

    if (this.rain) {
      const p = this.rainPos;
      const px = this.game.player?.root.position.x ?? 0;
      const pz = this.game.player?.root.position.z ?? 0;
      for (let i = 0; i < p.length / 3; i++) {
        p[i * 3 + 1] -= 26 * dt;
        p[i * 3] += (3.5 + Math.sin(t * 0.6) * 2.4) * dt;
        if (p[i * 3 + 1] < 0 || p[i * 3] - px > 30 || p[i * 3] - px < -30 || p[i * 3 + 2] - pz > 30 || p[i * 3 + 2] - pz < -30) {
          p[i * 3] = px + rand(-30, 30);
          p[i * 3 + 1] = rand(24, 40);
          p[i * 3 + 2] = pz + rand(-30, 30);
        }
      }
      this.rain.geometry.attributes.position.needsUpdate = true;
    }

    if (this.flickers) {
      for (const f of this.flickers) {
        const v = Math.sin(t * 7 + f.seed) * Math.sin(t * 13.7 + f.seed * 2);
        f.mat.color.setScalar(v < -0.82 ? rand(0.05, 0.25) : 1);
      }
    }

    if (this.billboards) {
      for (const tex of this.billboards) tex.offset.y = (tex.offset.y - dt * 0.045) % 1;
    }

    if (this.beacons) {
      for (let i = 0; i < this.beacons.length; i++) {
        this.beacons[i].material.color.setScalar(0);
        this.beacons[i].material.color.setRGB(Math.sin(t * 2.4 + i) > 0.4 ? 1 : 0.12, 0.05, 0.07);
      }
    }

    if (this.pylonMeshes) {
      for (const g of this.pylonMeshes) g.rotation.y += dt * 0.5;
    }

    if (this.peds) {
      const vc = this.game.vehicle;
      const ac = vc && vc.driving ? vc.active.group.position : null;
      const aspd = vc && vc.driving ? vc.active.vel.length() : 0;
      for (const ped of this.peds) {
        if (ac && aspd > 3) {
          const dx = ped.g.position.x - ac.x, dz = ped.g.position.z - ac.z;
          const d = Math.hypot(dx, dz);
          if (d < 7) {
            ped.wait = 0;
            ped.target = {
              x: Math.max(-190, Math.min(190, ped.g.position.x + dx / (d || 1) * 12)),
              z: Math.max(-190, Math.min(190, ped.g.position.z + dz / (d || 1) * 12))
            };
          }
        }
        if (ped.wait > 0) {
          ped.wait -= dt;
          continue;
        }
        const dx = ped.target.x - ped.g.position.x, dz = ped.target.z - ped.g.position.z;
        const d = Math.hypot(dx, dz);
        if (d < 0.3) {
          ped.wait = rand(0.5, 3);
          ped.target = this.pedTarget(ped.spec);
          continue;
        }
        ped.g.position.x += dx / d * ped.spec.speed * dt;
        ped.g.position.z += dz / d * ped.spec.speed * dt;
        ped.g.rotation.y = Math.atan2(dx, dz);
        ped.phase += dt * 9;
        ped.g.position.y = Math.abs(Math.sin(ped.phase)) * 0.04;
      }
    }

    if (this.flyers) {
      for (const f of this.flyers) {
        f.g.position.x += Math.cos(f.spec.dir) * f.spec.speed * dt;
        f.g.position.z += Math.sin(f.spec.dir) * f.spec.speed * dt;
        f.g.position.y = f.spec.y + Math.sin(t * 0.7 + f.ph) * 1.2;
        if (Math.abs(f.g.position.x) > 250) f.g.position.x = -Math.sign(f.g.position.x) * 245;
        if (Math.abs(f.g.position.z) > 250) f.g.position.z = -Math.sign(f.g.position.z) * 245;
        f.beacon.material.color.setRGB(1, 0.84, 0.04);
        f.beacon.visible = Math.sin(t * 4 + f.ph) > 0;
      }
    }
  }

  /* ---------------- collision ---------------- */

  collideMove(pos, r, dx, dz, minH = 0) {
    pos.x += dx;
    for (const c of this.colliders) {
      if (c.h < minH) continue;
      if (pos.x > c.minX - r && pos.x < c.maxX + r && pos.z > c.minZ - r && pos.z < c.maxZ + r) {
        pos.x = dx > 0 ? c.minX - r : c.maxX + r;
      }
    }
    pos.z += dz;
    for (const c of this.colliders) {
      if (c.h < minH) continue;
      if (pos.x > c.minX - r && pos.x < c.maxX + r && pos.z > c.minZ - r && pos.z < c.maxZ + r) {
        pos.z = dz > 0 ? c.minZ - r : c.maxZ + r;
      }
    }
    pos.x = Math.max(-BOUND, Math.min(BOUND, pos.x));
    pos.z = Math.max(-BOUND, Math.min(BOUND, pos.z));
  }

  segBlocked(ax, ay, az, bx, by, bz) {
    const dx = bx - ax, dy = by - ay, dz = bz - az;
    for (const c of this.colliders) {
      if (c.h < 1.8) continue;
      let t0 = 0, t1 = 1;
      let ok = true;
      const axis = (o, d, lo, hi) => {
        if (Math.abs(d) < 1e-9) { if (o < lo || o > hi) ok = false; return; }
        let ta = (lo - o) / d, tb = (hi - o) / d;
        if (ta > tb) [ta, tb] = [tb, ta];
        t0 = Math.max(t0, ta);
        t1 = Math.min(t1, tb);
        if (t0 > t1) ok = false;
      };
      axis(ax, dx, c.minX, c.maxX);
      if (ok) axis(ay, dy, 0, c.h);
      if (ok) axis(az, dz, c.minZ, c.maxZ);
      if (ok) return true;
    }
    return false;
  }

  raycast(o, d, maxDist) {
    let best = maxDist;
    let hit = null;
    for (const c of this.colliders) {
      let t0 = 0, t1 = best;
      let ok = true;
      const axis = (ov, dv, lo, hi) => {
        if (Math.abs(dv) < 1e-9) { if (ov < lo || ov > hi) ok = false; return; }
        let ta = (lo - ov) / dv, tb = (hi - ov) / dv;
        if (ta > tb) [ta, tb] = [tb, ta];
        t0 = Math.max(t0, ta);
        t1 = Math.min(t1, tb);
        if (t0 > t1) ok = false;
      };
      axis(o.x, d.x, c.minX, c.maxX);
      if (ok) axis(o.y, d.y, 0, c.h);
      if (ok) axis(o.z, d.z, c.minZ, c.maxZ);
      if (ok && t0 > 0.01 && t0 < best) {
        best = t0;
        hit = t0;
      }
    }
    if (d.y < -1e-6) {
      const tg = -o.y / d.y;
      if (tg > 0.01 && tg < best) { best = tg; hit = tg; }
    }
    if (hit === null) return null;
    return { t: best, point: new THREE.Vector3().copy(o).addScaledVector(d, best) };
  }

  pointBlocked(x, z, r = 0.7) {
    for (const c of this.colliders) {
      if (x > c.minX - r && x < c.maxX + r && z > c.minZ - r && z < c.maxZ + r) return true;
    }
    return false;
  }

  findClearPos(cx, cz, rmin, rmax) {
    for (let i = 0; i < 60; i++) {
      const a = rand(0, Math.PI * 2);
      const d = rand(rmin, rmax);
      const x = cx + Math.cos(a) * d, z = cz + Math.sin(a) * d;
      if (Math.abs(x) < BOUND - 3 && Math.abs(z) < BOUND - 3 && !this.pointBlocked(x, z)) {
        return { x, z };
      }
    }
    return { x: cx, z: cz };
  }
}
