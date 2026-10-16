extends CanvasLayer

var game
var hp_label: Label
var ammo_label: Label
var obj_label: Label
var toast_label: Label
var menu_label: Label
var win_label: Label
var toast_t := 0.0
var dlg := {}
var dlg_speaker: Label
var dlg_text: Label
var dlg_root: Control
var shop_root: PanelContainer
var shop_list: VBoxContainer
var shop_eddies: Label

const UPGRADES := [
	{"id": "plating", "name": "SUBDERMAL PLATING", "desc": "+25 max integrity per tier", "base": 300},
	{"id": "reflex", "name": "REFLEX BOOSTER", "desc": "+8% move speed per tier", "base": 350},
	{"id": "target", "name": "SMART TARGETING", "desc": "+12% weapon damage per tier", "base": 400},
	{"id": "nerve", "name": "NERVE WEAVE", "desc": "8% less damage taken per tier", "base": 250}
]

func setup(g) -> void:
	game = g
	layer = 10
	hp_label = _label(Vector2(20, 16), 18, Color(0.6, 0.95, 1.0))
	ammo_label = _label(Vector2(20, 48), 18, Color(1.0, 0.84, 0.1))
	obj_label = _label(Vector2(20, 80), 15, Color(0.8, 0.86, 0.92))
	toast_label = _label(Vector2(340, 110), 22, Color(1, 1, 1))
	toast_label.visible = false
	menu_label = _label(Vector2(330, 250), 36, Color(0.24, 0.95, 1.0))
	menu_label.text = "SEKTOR 7\n\nCLICK OR PRESS ENTER TO START\nGODOT PORT — MILESTONE 2"
	win_label = _label(Vector2(300, 230), 32, Color(0.9, 1.0, 1.0))
	win_label.visible = false
	_build_dialogue()
	_build_shop()

func _label(pos: Vector2, size: int, col: Color) -> Label:
	var l := Label.new()
	l.position = pos
	l.add_theme_font_size_override("font_size", size)
	l.add_theme_color_override("font_color", col)
	add_child(l)
	return l

func _build_dialogue() -> void:
	dlg_root = Control.new()
	dlg_root.position = Vector2(300, 520)
	dlg_root.custom_minimum_size = Vector2(680, 130)
	var panel := PanelContainer.new()
	dlg_root.add_child(panel)
	var vb := VBoxContainer.new()
	panel.add_child(vb)
	dlg_speaker = Label.new()
	dlg_speaker.add_theme_font_size_override("font_size", 16)
	dlg_speaker.add_theme_color_override("font_color", Color(1.0, 0.16, 0.43))
	vb.add_child(dlg_speaker)
	dlg_text = Label.new()
	dlg_text.add_theme_font_size_override("font_size", 17)
	dlg_text.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	dlg_text.custom_minimum_size = Vector2(640, 60)
	vb.add_child(dlg_text)
	var hint := Label.new()
	hint.text = "[E] CONTINUE"
	hint.add_theme_font_size_override("font_size", 12)
	hint.add_theme_color_override("font_color", Color(0.4, 0.55, 0.62))
	vb.add_child(hint)
	dlg_root.visible = false
	add_child(dlg_root)

func _build_shop() -> void:
	shop_root = PanelContainer.new()
	shop_root.position = Vector2(320, 140)
	shop_root.custom_minimum_size = Vector2(640, 400)
	var vb := VBoxContainer.new()
	shop_root.add_child(vb)
	var title := Label.new()
	title.text = "DOC CHROME — RIPPERDOC"
	title.add_theme_font_size_override("font_size", 22)
	title.add_theme_color_override("font_color", Color(1.0, 0.16, 0.43))
	vb.add_child(title)
	shop_eddies = Label.new()
	shop_eddies.add_theme_font_size_override("font_size", 16)
	shop_eddies.add_theme_color_override("font_color", Color(1.0, 0.84, 0.04))
	vb.add_child(shop_eddies)
	shop_list = VBoxContainer.new()
	vb.add_child(shop_list)
	var ammo_btn := Button.new()
	ammo_btn.text = "REFILL AMMO — ¢100"
	ammo_btn.pressed.connect(func():
		if game.player.eddies >= 100:
			game.player.eddies -= 100
			game.weapons.reserves[0] = maxi(game.weapons.reserves[0], 48)
			game.weapons.reserves[1] = maxi(game.weapons.reserves[1], 96)
			game.audio.sfx("ui")
			toast("AMMO RESUPPLIED", Color(1.0, 0.84, 0.04))
		_render_shop())
	vb.add_child(ammo_btn)
	var close_btn := Button.new()
	close_btn.text = "CLOSE [E]"
	close_btn.pressed.connect(func(): game.close_shop())
	vb.add_child(close_btn)
	shop_root.visible = false
	add_child(shop_root)

func dialogue(speaker: String, lines, cb = null) -> void:
	dlg = {"speaker": speaker, "lines": lines, "i": 0, "chars": 0.0, "cb": cb}
	dlg_speaker.text = speaker
	dlg_text.text = ""
	dlg_root.visible = true
	game.audio.sfx("ui")

func dialogue_active() -> bool:
	return not dlg.is_empty()

func advance_dialogue() -> void:
	if dlg.is_empty():
		return
	var line: String = dlg["lines"][dlg["i"]]
	if int(dlg["chars"]) < line.length():
		dlg["chars"] = float(line.length())
		return
	dlg["i"] = int(dlg["i"]) + 1
	dlg["chars"] = 0.0
	if int(dlg["i"]) >= dlg["lines"].size():
		var cb = dlg["cb"]
		dlg = {}
		dlg_root.visible = false
		if cb != null:
			cb.call()

func open_shop() -> void:
	_render_shop()
	shop_root.visible = true

func close_shop() -> void:
	shop_root.visible = false

func _render_shop() -> void:
	for child in shop_list.get_children():
		child.free()
	shop_eddies.text = "AVAILABLE  ¢ %d" % game.player.eddies
	for u in UPGRADES:
		var id: String = u["id"]
		var tier := int(game.upgrade_tiers.get(id, 0))
		var row := HBoxContainer.new()
		var name_l := Label.new()
		name_l.text = "%s — %s" % [u["name"], u["desc"]]
		name_l.add_theme_font_size_override("font_size", 14)
		name_l.custom_minimum_size = Vector2(430, 0)
		name_l.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
		row.add_child(name_l)
		var pips := Label.new()
		pips.text = "◆".repeat(tier) + "◇".repeat(3 - tier)
		pips.add_theme_font_size_override("font_size", 14)
		row.add_child(pips)
		var maxed := tier >= 3
		var price := int(round(float(u["base"]) * pow(1.7, tier)))
		var btn := Button.new()
		btn.text = "MAX" if maxed else "¢ %d" % price
		btn.disabled = maxed or game.player.eddies < price
		btn.pressed.connect(func(): buy(id))
		row.add_child(btn)
		shop_list.add_child(row)

func buy(id: String) -> void:
	for u in UPGRADES:
		if u["id"] != id:
			continue
		var tier := int(game.upgrade_tiers.get(id, 0))
		if tier >= 3:
			return
		var price := int(round(float(u["base"]) * pow(1.7, tier)))
		if game.player.eddies < price:
			game.audio.sfx("ui")
			toast("INSUFFICIENT EDDIES", Color(1.0, 0.28, 0.34))
			return
		game.player.eddies -= price
		game.upgrade_tiers[id] = tier + 1
		match id:
			"plating":
				game.player.max_hp += 25
				game.player.heal(25)
			"reflex":
				game.player.speed_mult *= 1.08
			"target":
				game.player.dmg_mult *= 1.12
			"nerve":
				game.player.armor_mult *= 0.92
		game.audio.sfx("level", 0.5)
		toast("CHROME INSTALLED", Color(0.22, 1.0, 0.53))
		_render_shop()
		return

func show_win(kills: int, eddies: int, level: int) -> void:
	win_label.text = "SEKTOR 7 CLEARED\n\nKILLS %d    EARNED ¢%d    LVL %d\n\nPRESS ENTER TO KEEP EXPLORING" % [kills, eddies, level]
	win_label.visible = true

func hide_win() -> void:
	win_label.visible = false

func toast(text: String, col: Color) -> void:
	toast_label.text = text
	toast_label.add_theme_color_override("font_color", col)
	toast_t = 2.2
	toast_label.visible = true

func _process(delta: float) -> void:
	if game == null or game.player == null:
		return
	hp_label.text = "HP %d/%d    LVL %d    ¢ %d    KILLS %d" % [
		game.player.hp, game.player.max_hp, game.player.level,
		game.player.eddies, game.stats["kills"]]
	if game.weapons != null:
		ammo_label.text = "%s   AMMO %d / %d%s" % [
			game.weapons.defs[game.weapons.current]["name"],
			game.weapons.mags[game.weapons.current],
			game.weapons.reserves[game.weapons.current],
			"  (RELOADING)" if game.weapons.reloading else ""]
	if game.quests != null:
		var info: Dictionary = game.quests.objective_info()
		obj_label.text = "%s\n%s\n%s" % [info["title"], info["task"], info["prog"]]
	menu_label.visible = game.state == "menu"
	if not dlg.is_empty():
		dlg["chars"] = float(dlg["chars"]) + 55.0 * delta
		var line: String = dlg["lines"][dlg["i"]]
		var shown := line.substr(0, mini(int(dlg["chars"]), line.length()))
		if dlg_text.text != shown:
			dlg_text.text = shown
	if toast_t > 0.0:
		toast_t -= delta
		toast_label.visible = toast_t > 0.0
