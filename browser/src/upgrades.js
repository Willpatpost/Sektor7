export const UPGRADES = [
  {
    id: 'plating', name: 'SUBDERMAL PLATING', desc: '+25 max integrity per tier',
    base: 300,
    apply(g) { g.player.maxHp += 25; g.player.heal(25); }
  },
  {
    id: 'reflex', name: 'REFLEX BOOSTER', desc: '+8% movement speed per tier',
    base: 350,
    apply(g) { g.player.speedMult *= 1.08; }
  },
  {
    id: 'target', name: 'SMART TARGETING', desc: '+12% weapon damage per tier',
    base: 400,
    apply(g) { g.player.dmgMult *= 1.12; }
  },
  {
    id: 'mito', name: 'MITO BOOSTER', desc: '+20 stamina per tier',
    base: 250,
    apply(g) { g.player.maxStamina += 20; g.player.stamina = g.player.maxStamina; }
  }
];

export function upgradePrice(u, tier) {
  return Math.round(u.base * Math.pow(1.7, tier));
}

export function upgradeTier(game, id) {
  return (game.upgradeTiers?.[id]) || 0;
}

export function buyUpgrade(game, id) {
  const u = UPGRADES.find(x => x.id === id);
  if (!u) return false;
  game.upgradeTiers ||= {};
  const tier = game.upgradeTiers[id] || 0;
  if (tier >= 3) return false;
  const cost = upgradePrice(u, tier);
  if (game.player.eddies < cost) return false;
  game.player.eddies -= cost;
  game.upgradeTiers[id] = tier + 1;
  u.apply(game);
  return true;
}
