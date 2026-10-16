import * as THREE from 'three';

export const JOBS = [
  {
    title: 'WET WORK', marker: 'plaza', kind: 'kill', targetType: 'merc', count: 4,
    objective: 'Neutralize the Rogue-Signal mercs holding KANEDA PLAZA',
    reward: { eddies: 400, xp: 200, unlockSMG: true },
    brief: [
      'V. Good — you picked up. Call me VEX. I broker work in Sektor 7.',
      'Rogue-Signal mercs just tagged KANEDA PLAZA like they own the district.',
      'Four of them. Loud, ugly, armed.',
      'Clear the plaza. ¢400 says nobody misses them.',
      'One more thing — see DOC CHROME. His terminal is on your block. Kit up.'
    ],
    done: [
      'Plaza breathes again. ¢400 wired to your account.',
      'Those mercs took contracts from HELIX DYNAMICS. A corp.',
      'HELIX just noticed you exist. Wait for my ping.'
    ]
  },
  {
    title: 'COLD STORAGE', marker: 'lockup', kind: 'shard',
    objective: 'Recover the HELIX data shard from the lockup',
    reward: { eddies: 500, xp: 250 },
    brief: [
      'A HELIX transport dumped a data shard into the old lockup, northwest side.',
      'It is still in the crate by the loading door.',
      'They posted a security detail. Light them up if they blink at you.',
      'Bring me the shard. ¢500.'
    ],
    done: [
      'Shard reads clean… V, this thing is a roster. Every merc in the district.',
      'HELIX knows what they lost. They are watching the skies now.'
    ]
  },
  {
    title: 'AIR FREIGHT', marker: 'market', kind: 'kill', targetType: 'drone', count: 4,
    objective: 'Down the HELIX recon drones over MOTO MARKET',
    reward: { eddies: 600, xp: 300 },
    brief: [
      'HELIX drones are circling MOTO MARKET, mapping faces.',
      'Four of them, up high. They burn easy.',
      '¢600. Keep the sky clean, V.'
    ],
    done: [
      'Market is dark to their optics now.',
      'The merc captain goes by RAZORTAIL. She parked her crew in the east plaza.',
      'Last job, V. The big one.'
    ]
  },
  {
    title: 'SEKTOR 7', marker: 'boss', kind: 'kill', targetType: 'boss', count: 1,
    objective: 'Eliminate RAZORTAIL — the Rogue-Signal captain',
    reward: { eddies: 1000, xp: 500 },
    brief: [
      'RAZORTAIL runs Rogue-Signal\u2019s whole Sektor 7 operation.',
      'Two bodyguards. Her? Six hundred pounds of chrome and bad decisions.',
      'Take her off the board and this district remembers your name.',
      '¢1000. Make it loud.'
    ],
    done: [
      'Razortail\u2019s flatline is all over the nets already.',
      'SEKTOR 7 CLEARED, V. You are somebody now.'
    ]
  }
];

export class QuestSystem {
  constructor(game) {
    this.game = game;
    this.idx = -1;
    this.active = false;
    this.progress = 0;
    this.allDone = false;
    this.pending = null;
    this.shardMesh = null;
    this.shardInteract = null;
    this.started = false;
  }

  begin(i) {
    this.idx = i;
    this.active = true;
    this.progress = 0;
    this.started = true;
    const job = JOBS[i];
    if (this.game.hud) this.game.hud.dialogue('VEX', job.brief);
    this.spawnJob(job);
    this.game.audio?.sfx('quest');
  }

  spawnJob(job) {
    const w = this.game.world;
    const em = this.game.enemies;
    const poi = w.pois[job.marker];

    if (job.marker === 'plaza') {
      for (let i = 0; i < job.count; i++) {
        const pos = w.findClearPos(poi.x, poi.z, 3, 12);
        em.spawnMerc(pos.x, pos.z, { aggressive: true });
      }
    } else if (job.marker === 'lockup') {
      for (let i = 0; i < 3; i++) {
        const pos = w.findClearPos(poi.x, poi.z, 4, 10);
        em.spawnMerc(pos.x, pos.z, { aggressive: true });
      }
      em.spawnDrone(poi.x, poi.z - 6, { aggressive: true });
      const shard = new THREE.Mesh(
        new THREE.OctahedronGeometry(0.3, 0),
        new THREE.MeshBasicMaterial({ color: 0xffd60a, toneMapped: false })
      );
      shard.position.set(poi.x, 0.9, poi.z + 0.6);
      this.game.scene.add(shard);
      this.shardMesh = shard;
      this.shardInteract = {
        id: 'shard',
        x: poi.x, z: poi.z + 0.6, radius: 2.6,
        prompt: 'RECOVER DATA SHARD',
        action: () => this.collectShard()
      };
      this.game.addInteractable?.(this.shardInteract);
    } else if (job.marker === 'market') {
      for (let i = 0; i < job.count; i++) {
        const pos = w.findClearPos(poi.x, poi.z, 4, 12);
        em.spawnDrone(pos.x, pos.z, { aggressive: true });
      }
    } else if (job.marker === 'boss') {
      em.spawnBoss(poi.x, poi.z);
      for (let i = 0; i < 2; i++) {
        const pos = w.findClearPos(poi.x, poi.z, 3, 8);
        em.spawnMerc(pos.x, pos.z, { aggressive: true });
      }
    }
  }

  onKill(type) {
    if (!this.active) return;
    const job = JOBS[this.idx];
    if (job.kind === 'kill' && job.targetType === type) {
      this.progress++;
      if (this.progress >= job.count) this.complete();
    }
  }

  collectShard() {
    if (!this.active || JOBS[this.idx].kind !== 'shard') return;
    if (this.shardMesh) {
      this.game.scene.remove(this.shardMesh);
      this.shardMesh = null;
    }
    if (this.shardInteract) {
      this.game.removeInteractable?.(this.shardInteract);
      this.shardInteract = null;
    }
    this.complete();
  }

  complete() {
    this.active = false;
    const job = JOBS[this.idx];
    const g = this.game;
    g.addEddies?.(job.reward.eddies);
    g.player.addXP(job.reward.xp);
    g.hud?.toast(`JOB COMPLETE // +¢${job.reward.eddies}`, '#ffd60a');
    g.audio?.sfx('quest');
    if (job.reward.unlockSMG) g.weapons?.unlock(1);

    const after = () => {
      if (this.idx < JOBS.length - 1) {
        this.pending = { next: this.idx + 1, t: 4 };
      } else {
        this.allDone = true;
        g.win?.();
      }
    };
    if (g.hud) g.hud.dialogue('VEX', job.done, after);
    else after();
  }

  update(dt) {
    if (this.pending) {
      this.pending.t -= dt;
      if (this.pending.t <= 0) {
        const n = this.pending.next;
        this.pending = null;
        this.begin(n);
      }
    }
    if (this.shardMesh) {
      this.shardMesh.rotation.y += dt * 2;
      this.shardMesh.position.y = 0.9 + Math.sin(this.game.time * 2.4) * 0.15;
    }
  }

  objectiveInfo() {
    if (this.allDone) return { title: 'SEKTOR 7', task: 'District cleared — free roam. Remnants still roam.', prog: '' };
    if (!this.active) return { title: 'STANDBY', task: 'Awaiting VEX on comms…', prog: '' };
    const job = JOBS[this.idx];
    let prog = '';
    if (job.kind === 'kill') prog = `TARGETS DOWN ${this.progress} / ${job.count}`;
    else if (job.kind === 'shard') prog = '◈ RECOVER THE SHARD';
    return { title: `JOB ${this.idx + 1} // ${job.title}`, task: job.objective, prog };
  }

  markerPos() {
    if (!this.active) return null;
    return this.game.world.pois[JOBS[this.idx].marker];
  }
}
