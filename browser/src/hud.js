import { UPGRADES, upgradePrice, upgradeTier, buyUpgrade } from './upgrades.js';

const SPEAKERS = {
  VEX: '#ff2a6d',
  'DOC CHROME': '#3ef2ff',
  RAZORTAIL: '#ff5a5a'
};

export class HUD {
  constructor(game) {
    this.game = game;
    const $ = (id) => document.getElementById(id);
    this.el = {
      objtitle: $('objtitle'), objtask: $('objtask'), objprog: $('objprog'),
      minimap: $('minimap'), mmn: $('mmn'),
      hpbar: $('hpbar'), hpfill: $('hpfill'), stamfill: $('stamfill'),
      lvl: $('lvl'), eddies: $('eddies'),
      wname: $('wname'), wammo: $('wammo'), whint: $('whint'),
      vehicle: $('vehicle'), vname: $('vname'), vspeed: $('vspeed'),
      crosshair: $('crosshair'), hitmarker: $('hitmarker'),
      mdist: $('mdist'), interact: $('interact'), interacttext: $('interacttext'),
      toasts: $('toasts'), dialogue: $('dialogue'), dspeaker: $('dspeaker'),
      dtext: $('dtext'), dhint: $('dhint'),
      vignette: $('vignette'), flash: $('flash'),
      menu: $('menu'), pause: $('pause'), death: $('death'), win: $('win'),
      winstats: $('winstats'),
      shop: $('shop'), shoplist: $('shoplist'), shopeddies: $('shopeddies'),
      ammobtn: $('ammobtn'), shopclose: $('shopclose')
    };
    this.el.mmn.style.display = 'none';
    this.mm = this.el.minimap.getContext('2d');
    this.dlg = null;
    this.interactPending = null;
    this.mdistPending = null;
    this.hitT = 0;
    this.shopSel = 0;

    this.el.ammobtn.onclick = () => {
      const g = this.game;
      if (g.player.eddies >= 100) {
        g.player.eddies -= 100;
        g.weapons.refillAmmo();
        g.audio?.sfx('reload');
        this.toast('AMMO RESUPPLIED', '#ffd60a');
      } else {
        g.audio?.sfx('ui');
        this.toast('INSUFFICIENT EDDIES', '#ff4757');
      }
      this.renderShop();
    };
    this.el.shopclose.onclick = () => this.game.closeShop?.();
  }

  update(dt) {
    const g = this.game;
    const p = g.player;
    if (!p) return;

    const hpF = Math.max(0, p.hp / p.maxHp);
    this.el.hpfill.style.width = (hpF * 100).toFixed(1) + '%';
    this.el.hpbar.classList.toggle('low', hpF < 0.35);
    this.el.stamfill.style.width = (p.stamina / p.maxStamina * 100).toFixed(1) + '%';
    this.el.lvl.textContent = `LVL ${p.level} ▸ ${p.xp}/${p.xpNext} XP`;
    this.el.eddies.textContent = `¢ ${p.eddies.toLocaleString()}`;
    this.el.vignette.classList.toggle('lowhp', hpF < 0.3);

    this.updateAmmo();
    const veh = g.vehicle;
    if (g.player.inCar && veh && veh.active) {
      this.el.vehicle.style.display = 'block';
      this.el.vname.textContent = `${veh.active.name} · ALT ${Math.round(veh.active.group.position.y)}M`;
      this.el.vspeed.innerHTML = `${Math.round(veh.active.vel.length() * 3.6)} <small>KM/H</small>`;
    } else {
      this.el.vehicle.style.display = 'none';
    }
    this.el.crosshair.style.setProperty('--gap', `${(g.weapons?.spreadPx || 4).toFixed(1)}px`);
    this.el.crosshair.style.display = g.pov?.front ? 'none' : 'block';

    const info = g.quests.objectiveInfo();
    this.el.objtitle.textContent = info.title;
    this.el.objtask.textContent = info.task;
    this.el.objprog.textContent = info.prog;

    if (this.mdistPending != null) {
      this.el.mdist.style.display = 'block';
      this.el.mdist.textContent = `◈ ${Math.round(this.mdistPending)}m`;
    } else {
      this.el.mdist.style.display = 'none';
    }

    if (this.interactPending) {
      this.el.interact.style.display = 'block';
      this.el.interacttext.textContent = this.interactPending;
    } else {
      this.el.interact.style.display = 'none';
    }

    if (this.dlg) {
      this.dlg.chars += dt * 52;
      const line = this.dlg.lines[this.dlg.i];
      const shown = line.slice(0, Math.floor(this.dlg.chars));
      if (this.el.dtext.textContent !== shown) this.el.dtext.textContent = shown;
    }

    if (this.hitT > 0) {
      this.hitT -= dt;
      if (this.hitT <= 0) this.el.hitmarker.classList.remove('show', 'kill');
    }

    this.drawMinimap();
  }

  updateAmmo() {
    const w = this.game.weapons;
    if (!w) return;
    this.el.wname.textContent = w.def.name;
    if (w.reloading) {
      this.el.wammo.innerHTML = '<span style="color:#ffd60a">RELOADING</span>';
    } else {
      this.el.wammo.innerHTML = `${w.st.mag} <small>⁄ ${w.st.reserve}</small>`;
    }
    const names = ['KURO', 'WASP'];
    this.el.whint.textContent = w.state
      .map((s, i) => `[${i + 1}] ${s.unlocked ? names[i] : 'LOCKED'}`)
      .join(' · ');
  }

  drawMinimap() {
    const g = this.game;
    const p = g.player;
    const ctx = this.mm;
    const S = 172, C = S / 2, SC = 0.75;
    ctx.clearRect(0, 0, S, S);
    ctx.save();
    ctx.beginPath();
    ctx.arc(C, C, C - 3, 0, Math.PI * 2);
    ctx.clip();
    ctx.fillStyle = 'rgba(5,10,18,.9)';
    ctx.fillRect(0, 0, S, S);

    const px = p.root.position.x, pz = p.root.position.z;
    const yaw = p.root.rotation.y;
    ctx.translate(C, C);
    ctx.rotate(yaw);

    ctx.fillStyle = 'rgba(40,80,120,.5)';
    for (const c of g.world.colliders) {
      if (!c.mm) continue;
      const x = (c.minX - px) * SC, z = (c.minZ - pz) * SC;
      const w = (c.maxX - c.minX) * SC, h = (c.maxZ - c.minZ) * SC;
      if (x + w < -C || x > C || z + h < -C || z > C) continue;
      ctx.fillRect(x, z, w, h);
    }

    ctx.fillStyle = '#39ff88';
    for (const pk of g.enemies.pickups) {
      ctx.fillRect((pk.mesh.position.x - px) * SC - 1.5, (pk.mesh.position.z - pz) * SC - 1.5, 3, 3);
    }

    ctx.fillStyle = '#ff4757';
    for (const e of g.enemies.enemies) {
      if (!e.alive) continue;
      ctx.beginPath();
      ctx.arc((e.group.position.x - px) * SC, (e.group.position.z - pz) * SC, 2.6, 0, 7);
      ctx.fill();
    }

    const rd = g.world.ripperdoc;
    if (rd) {
      ctx.fillStyle = '#3ef2ff';
      ctx.fillRect((rd.x - px) * SC - 2.5, (rd.z - pz) * SC - 2.5, 5, 5);
    }

    const mp = g.quests.markerPos();
    if (mp) {
      ctx.fillStyle = '#ffd60a';
      ctx.save();
      ctx.translate((mp.x - px) * SC, (mp.z - pz) * SC);
      ctx.rotate(Math.PI / 4);
      ctx.fillRect(-3.5, -3.5, 7, 7);
      ctx.restore();
    }
    ctx.restore();

    ctx.save();
    ctx.translate(C, C);
    ctx.fillStyle = '#eaffff';
    ctx.shadowColor = '#3ef2ff';
    ctx.shadowBlur = 6;
    ctx.beginPath();
    ctx.moveTo(0, -6);
    ctx.lineTo(4.4, 5);
    ctx.lineTo(-4.4, 5);
    ctx.closePath();
    ctx.fill();
    ctx.restore();

    const nx = Math.sin(yaw) * (C - 13), ny = -Math.cos(yaw) * (C - 13);
    ctx.fillStyle = '#3ef2ff';
    ctx.font = 'bold 10px monospace';
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText('N', C + nx, C + ny);
  }

  /* ---------- dialogue ---------- */

  dialogue(speaker, lines, cb) {
    this.dlg = { speaker, lines, i: 0, chars: 0, cb };
    this.el.dialogue.style.display = 'block';
    this.el.dspeaker.textContent = speaker;
    this.el.dspeaker.style.color = SPEAKERS[speaker] || '#3ef2ff';
    this.el.dtext.textContent = '';
    this.game.audio?.sfx('ui');
  }

  dialogueActive() {
    return !!this.dlg;
  }

  advanceDialogue() {
    if (!this.dlg) return;
    const line = this.dlg.lines[this.dlg.i];
    if (Math.floor(this.dlg.chars) < line.length) {
      this.dlg.chars = line.length;
      return;
    }
    this.dlg.i++;
    this.dlg.chars = 0;
    if (this.dlg.i >= this.dlg.lines.length) {
      const cb = this.dlg.cb;
      this.dlg = null;
      this.el.dialogue.style.display = 'none';
      cb?.();
    }
  }

  /* ---------- feedback ---------- */

  toast(msg, color) {
    const d = document.createElement('div');
    d.className = 'toast';
    d.style.color = color || '#eaffff';
    d.textContent = msg;
    this.el.toasts.appendChild(d);
    setTimeout(() => d.remove(), 2900);
  }

  hitmark(kill) {
    this.el.hitmarker.classList.add('show');
    this.el.hitmarker.classList.toggle('kill', !!kill);
    this.hitT = 0.09;
  }

  damageFlash(n) {
    this.el.flash.style.opacity = String(Math.min(0.7, n / 24));
    clearTimeout(this._flashTO);
    this._flashTO = setTimeout(() => { this.el.flash.style.opacity = '0'; }, 80);
  }

  setInteract(text) {
    this.interactPending = text;
  }

  setMarkerDist(d) {
    this.mdistPending = d;
  }

  /* ---------- overlays ---------- */

  show(id) {
    this.el[id].classList.add('on');
  }

  hide(id) {
    this.el[id].classList.remove('on');
  }

  showWin(stats) {
    this.el.winstats.innerHTML =
      `TIME IN DISTRICT — <b>${stats.time}</b><br>` +
      `HOSTILES NEUTRALIZED — <b>${stats.kills}</b><br>` +
      `EDDIES EARNED — <b>¢ ${stats.eddiesEarned.toLocaleString()}</b><br>` +
      `FINAL LEVEL — <b>${stats.level}</b>`;
    this.show('win');
  }

  /* ---------- shop ---------- */

  showShop() {
    this.renderShop();
    this.show('shop');
  }

  renderShop() {
    const g = this.game;
    this.el.shopeddies.textContent = `¢ ${g.player.eddies.toLocaleString()}`;
    const rows = UPGRADES.map(u => {
      const tier = upgradeTier(g, u.id);
      const maxed = tier >= 3;
      const cost = upgradePrice(u, tier);
      const pips = '◆'.repeat(tier) + '◇'.repeat(3 - tier);
      const afford = g.player.eddies >= cost;
      return `<div class="shoprow">
        <div class="nm"><b>${u.name}</b><span>${u.desc}</span></div>
        <div class="pips">${pips}</div>
        <div class="price">${maxed ? 'MAX' : '¢ ' + cost.toLocaleString()}</div>
        <button class="btn sm buy" data-id="${u.id}" ${maxed || !afford ? 'disabled' : ''}>BUY</button>
      </div>`;
    }).join('');
    this.el.shoplist.innerHTML = rows;
    this.el.shoplist.querySelectorAll('.shoprow').forEach((r, i) =>
      r.classList.toggle('sel', i === this.shopSel));
    this.el.shoplist.querySelectorAll('.buy').forEach(b => {
      b.onclick = () => {
        if (buyUpgrade(this.game, b.dataset.id)) {
          this.game.audio?.sfx('level', 0.5);
          this.toast('CHROME INSTALLED', '#39ff88');
        } else {
          this.game.audio?.sfx('ui');
          this.toast('INSUFFICIENT EDDIES', '#ff4757');
        }
        this.renderShop();
      };
    });
  }

  shopMove(dir) {
    const rows = this.el.shoplist.querySelectorAll('.shoprow');
    const n = rows.length;
    if (!n) return;
    this.shopSel = (this.shopSel + dir + n) % n;
    rows.forEach((r, i) => r.classList.toggle('sel', i === this.shopSel));
    this.game.audio?.sfx('ui');
  }

  shopConfirm() {
    const btns = this.el.shoplist.querySelectorAll('.buy');
    const b = btns[this.shopSel];
    if (b && !b.disabled) b.click();
    else this.game.audio?.sfx('ui');
  }
}
