export class GamepadSys {
  constructor(game) {
    this.game = game;
    this.index = -1;
    this.connected = false;
    this.prev = {};
    this.edges = {};
    this.moveX = 0; this.moveY = 0; this.lookX = 0; this.lookY = 0;
    this.fire = false; this.sprint = false; this.jump = false;
    this.up = false; this.down = false;
    this.prevFire = false;
    this.stickUp = false; this.stickDown = false;
    this.DZ = 0.14;

    window.addEventListener('gamepadconnected', (e) => {
      this.index = e.gamepad.index;
      this.connected = true;
      game.hud?.toast('CONTROLLER LINKED', '#39ff88');
      game.audio?.sfx('quest', 0.6);
      const h = document.getElementById('padhint');
      if (h) h.style.display = 'block';
    });
    window.addEventListener('gamepaddisconnected', (e) => {
      if (e.gamepad.index !== this.index) return;
      this.connected = false;
      this.index = -1;
      this.moveX = this.moveY = this.lookX = this.lookY = 0;
      this.fire = this.sprint = this.jump = false;
      game.hud?.toast('CONTROLLER LOST', '#ff4757');
      const h = document.getElementById('padhint');
      if (h) h.style.display = 'none';
    });
  }

  pad() {
    if (!this.connected || !navigator.getGamepads) return null;
    const p = navigator.getGamepads()[this.index];
    if (!p) {
      this.connected = false;
      return null;
    }
    return p;
  }

  edge(i) {
    return !!this.edges[i];
  }

  update() {
    this.edges = {};
    const p = this.pad();
    if (!p) {
      this.moveX = this.moveY = this.lookX = this.lookY = 0;
      this.fire = this.sprint = this.jump = this.up = this.down = false;
      return;
    }
    const dz = (v) => {
      if (Math.abs(v) < this.DZ) return 0;
      return (v - Math.sign(v) * this.DZ) / (1 - this.DZ);
    };
    this.moveX = dz(p.axes[0] || 0);
    this.moveY = dz(p.axes[1] || 0);
    this.lookX = dz(p.axes[2] || 0);
    this.lookY = dz(p.axes[3] || 0);
    const rt = p.buttons[7];
    this.fire = !!(rt && (rt.pressed || rt.value > 0.45));
    this.sprint = !!(p.buttons[10] && p.buttons[10].pressed);
    this.jump = !!(p.buttons[0] && p.buttons[0].pressed);
    this.up = !!(p.buttons[5] && p.buttons[5].pressed);
    this.down = !!(p.buttons[4] && p.buttons[4].pressed);
    for (let i = 0; i < p.buttons.length; i++) {
      const pr = !!(p.buttons[i] && p.buttons[i].pressed);
      if (pr && !this.prev[i]) this.edges[i] = true;
      this.prev[i] = pr;
    }
  }
}
