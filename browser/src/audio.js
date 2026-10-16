export class AudioSys {
  constructor() {
    this.ctx = null;
    this.step = 0;
    this.nextNote = 0;
    this.SIX = 60 / 96 / 4;
    this.BASS = [0, null, 0, 3, 0, null, 7, null, 0, null, 0, 3, 10, null, 7, null];
    this.ROOTS = [0, -4, 3, -2];
  }

  init() {
    if (this.ctx) return;
    const AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) return;
    const ctx = this.ctx = new AC();
    this.master = ctx.createGain();
    this.master.gain.value = 0.5;
    this.master.connect(ctx.destination);
    this.sfxBus = ctx.createGain();
    this.sfxBus.gain.value = 0.85;
    this.sfxBus.connect(this.master);
    this.musBus = ctx.createGain();
    this.musBus.gain.value = 0;
    this.musBus.connect(this.master);

    const len = ctx.sampleRate;
    this.noiseBuf = ctx.createBuffer(1, len, ctx.sampleRate);
    const d = this.noiseBuf.getChannelData(0);
    for (let i = 0; i < len; i++) d[i] = Math.random() * 2 - 1;

    this.startAmbient();
    this.nextNote = ctx.currentTime + 0.1;
    this.musBus.gain.linearRampToValueAtTime(0.42, ctx.currentTime + 4);
  }

  resume() {
    if (this.ctx && this.ctx.state === 'suspended') this.ctx.resume();
  }

  osc(type, f0, f1, dur, vol, t0, bus) {
    const ctx = this.ctx;
    const o = ctx.createOscillator();
    o.type = type;
    o.frequency.setValueAtTime(Math.max(f0, 1), t0);
    if (f1) o.frequency.exponentialRampToValueAtTime(Math.max(f1, 1), t0 + dur);
    const g = ctx.createGain();
    g.gain.setValueAtTime(vol, t0);
    g.gain.exponentialRampToValueAtTime(0.001, t0 + dur);
    o.connect(g);
    g.connect(bus || this.sfxBus);
    o.start(t0);
    o.stop(t0 + dur + 0.03);
  }

  noise(dur, vol, type, f0, f1, t0, Q, bus) {
    const ctx = this.ctx;
    const s = ctx.createBufferSource();
    s.buffer = this.noiseBuf;
    s.loop = true;
    const fl = ctx.createBiquadFilter();
    fl.type = type;
    fl.frequency.setValueAtTime(Math.max(f0, 10), t0);
    if (f1) fl.frequency.exponentialRampToValueAtTime(Math.max(f1, 10), t0 + dur);
    fl.Q.value = Q || 1;
    const g = ctx.createGain();
    g.gain.setValueAtTime(vol, t0);
    g.gain.exponentialRampToValueAtTime(0.001, t0 + dur);
    s.connect(fl);
    fl.connect(g);
    g.connect(bus || this.sfxBus);
    s.start(t0);
    s.stop(t0 + dur + 0.05);
  }

  sfx(name, vol = 1, pos = null) {
    if (!this.ctx) return;
    if (pos && this.game?.player) {
      const p = this.game.player.root.position;
      const d = Math.hypot(pos.x - p.x, pos.y - p.y, pos.z - p.z);
      vol *= Math.max(0.06, 1 / (1 + d / 16));
    }
    if (vol <= 0.01) return;
    const t = this.ctx.currentTime;
    switch (name) {
      case 'shoot':
        this.noise(0.09, 0.5 * vol, 'bandpass', 1500, 700, t, 1.2);
        this.osc('square', 150, 55, 0.07, 0.32 * vol, t);
        break;
      case 'shoot_smg':
        this.noise(0.06, 0.3 * vol, 'bandpass', 1800, 900, t, 1.2);
        this.osc('square', 190, 80, 0.05, 0.2 * vol, t);
        break;
      case 'eshoot':
        this.osc('sawtooth', 820, 160, 0.13, 0.15 * vol, t);
        this.noise(0.08, 0.1 * vol, 'lowpass', 1200, 300, t);
        break;
      case 'laser':
        this.osc('sawtooth', 1400, 300, 0.16, 0.13 * vol, t);
        break;
      case 'hit':
        this.osc('square', 1250, 1150, 0.03, 0.13 * vol, t);
        break;
      case 'hurt':
        this.osc('sine', 110, 55, 0.2, 0.5 * vol, t);
        this.noise(0.15, 0.28 * vol, 'lowpass', 400, 100, t);
        break;
      case 'kill':
        this.osc('square', 620, 180, 0.16, 0.28 * vol, t);
        break;
      case 'boom':
        this.noise(0.5, 0.55 * vol, 'lowpass', 600, 60, t);
        this.osc('sine', 90, 35, 0.4, 0.5 * vol, t);
        break;
      case 'pickup':
        this.osc('sine', 780, 1560, 0.09, 0.2 * vol, t);
        this.osc('sine', 1170, 1560, 0.06, 0.15 * vol, t + 0.07);
        break;
      case 'reload':
        this.noise(0.03, 0.25 * vol, 'highpass', 2500, 0, t);
        this.noise(0.03, 0.2 * vol, 'highpass', 3200, 0, t + 0.13);
        break;
      case 'level':
        this.osc('triangle', 440, 0, 0.22, 0.22 * vol, t);
        this.osc('triangle', 587, 0, 0.22, 0.22 * vol, t + 0.1);
        this.osc('triangle', 880, 0, 0.3, 0.22 * vol, t + 0.2);
        break;
      case 'quest':
        this.osc('triangle', 523, 0, 0.16, 0.2 * vol, t);
        this.osc('triangle', 659, 0, 0.16, 0.2 * vol, t + 0.09);
        this.osc('triangle', 784, 0, 0.26, 0.2 * vol, t + 0.18);
        break;
      case 'win':
        [392, 523, 659, 784, 1046].forEach((f, i) =>
          this.osc('triangle', f, 0, 0.3, 0.2 * vol, t + i * 0.11));
        break;
      case 'death':
        this.osc('sawtooth', 220, 30, 0.9, 0.35 * vol, t);
        this.noise(0.8, 0.25 * vol, 'lowpass', 800, 60, t);
        break;
      case 'alert':
        this.osc('square', 1180, 1180, 0.05, 0.09 * vol, t);
        this.osc('square', 1180, 1180, 0.05, 0.09 * vol, t + 0.08);
        break;
      case 'ui':
        this.osc('square', 960, 940, 0.04, 0.1 * vol, t);
        break;
    }
  }

  engineStart() {
    if (!this.ctx || this.eng) return;
    const ctx = this.ctx;
    const o1 = ctx.createOscillator();
    o1.type = 'sawtooth';
    o1.frequency.value = 55;
    const o2 = ctx.createOscillator();
    o2.type = 'square';
    o2.frequency.value = 28;
    const fl = ctx.createBiquadFilter();
    fl.type = 'lowpass';
    fl.frequency.value = 320;
    const g = ctx.createGain();
    g.gain.value = 0.0001;
    o1.connect(fl);
    o2.connect(fl);
    fl.connect(g);
    g.connect(this.sfxBus);
    o1.start();
    o2.start();
    g.gain.linearRampToValueAtTime(0.07, ctx.currentTime + 0.4);
    this.eng = { o1, o2, fl, g };
  }

  engineUpdate(n) {
    if (!this.eng) return;
    const t = this.ctx.currentTime;
    const gear = (n * 3.1) % 1;
    this.eng.o1.frequency.setTargetAtTime(50 + gear * 130 + n * 65, t, 0.05);
    this.eng.o2.frequency.setTargetAtTime(25 + gear * 65 + n * 32, t, 0.05);
    this.eng.fl.frequency.setTargetAtTime(300 + n * 1100, t, 0.08);
    this.eng.g.gain.setTargetAtTime(0.05 + n * 0.075, t, 0.1);
  }

  engineStop() {
    if (!this.eng) return;
    const e = this.eng;
    this.eng = null;
    const t = this.ctx.currentTime;
    e.g.gain.setTargetAtTime(0.0001, t, 0.15);
    e.o1.stop(t + 1);
    e.o2.stop(t + 1);
  }

  startAmbient() {
    const ctx = this.ctx;
    const rain = ctx.createBufferSource();
    rain.buffer = this.noiseBuf;
    rain.loop = true;
    const lp = ctx.createBiquadFilter();
    lp.type = 'lowpass';
    lp.frequency.value = 750;
    const g = ctx.createGain();
    g.gain.value = 0.045;
    rain.connect(lp); lp.connect(g); g.connect(this.master);
    rain.start();

    const hum = ctx.createGain();
    hum.gain.value = 0.016;
    hum.connect(this.master);
    [55, 110.7].forEach((f, i) => {
      const o = ctx.createOscillator();
      o.type = 'triangle';
      o.frequency.value = f;
      const og = ctx.createGain();
      og.gain.value = i ? 0.5 : 1;
      o.connect(og); og.connect(hum);
      o.start();
    });
  }

  update() {
    if (!this.ctx || this.ctx.state !== 'running') return;
    const t = this.ctx.currentTime;
    if (this.nextNote < t) this.nextNote = t + 0.05;
    while (this.nextNote < t + 0.18) {
      this.scheduleStep(this.step, this.nextNote);
      this.step = (this.step + 1) % 64;
      this.nextNote += this.SIX;
    }
  }

  scheduleStep(s, t) {
    if (s % 4 === 0) this.osc('sine', 130, 40, 0.13, 0.5, t, this.musBus);
    if (s % 4 === 2) this.noise(0.04, 0.09, 'highpass', 7000, 0, t, 1, this.musBus);
    if (s % 16 === 12) this.noise(0.11, 0.14, 'bandpass', 1700, 900, t, 1.4, this.musBus);

    const semi = this.BASS[s % 16];
    if (semi !== null) {
      const root = this.ROOTS[Math.floor(s / 16)];
      const f = 55 * Math.pow(2, (root + semi) / 12);
      const ctx = this.ctx;
      const o = ctx.createOscillator();
      o.type = 'sawtooth';
      o.frequency.value = f;
      const fl = ctx.createBiquadFilter();
      fl.type = 'lowpass';
      fl.frequency.value = 320;
      const g = ctx.createGain();
      g.gain.setValueAtTime(0.14, t);
      g.gain.exponentialRampToValueAtTime(0.001, t + 0.13);
      o.connect(fl); fl.connect(g); g.connect(this.musBus);
      o.start(t); o.stop(t + 0.16);
    }

    if (s % 16 === 0) {
      const root = this.ROOTS[Math.floor(s / 16)];
      const ctx = this.ctx;
      [0, 3, 7].forEach((semi) => {
        const f = 110 * Math.pow(2, (root + semi) / 12);
        [-4, 4].forEach((det) => {
          const o = ctx.createOscillator();
          o.type = 'sawtooth';
          o.frequency.value = f;
          o.detune.value = det;
          const fl = ctx.createBiquadFilter();
          fl.type = 'lowpass';
          fl.frequency.value = 460;
          const g = ctx.createGain();
          const dur = this.SIX * 15;
          g.gain.setValueAtTime(0.0001, t);
          g.gain.linearRampToValueAtTime(0.028, t + 0.6);
          g.gain.setValueAtTime(0.028, t + dur - 0.5);
          g.gain.linearRampToValueAtTime(0.0001, t + dur);
          o.connect(fl); fl.connect(g); g.connect(this.musBus);
          o.start(t); o.stop(t + dur + 0.1);
        });
      });
    }
  }
}
