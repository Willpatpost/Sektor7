extends CanvasLayer

var game
var hp_label: Label
var ammo_label: Label
var obj_label: Label
var toast_label: Label
var menu_label: Label
var toast_t := 0.0

func setup(g: Node3D) -> void:
	game = g
	layer = 10
	hp_label = _label(Vector2(20, 16), 18, Color(0.6, 0.95, 1.0))
	ammo_label = _label(Vector2(20, 48), 18, Color(1.0, 0.84, 0.1))
	obj_label = _label(Vector2(20, 80), 15, Color(0.8, 0.86, 0.92))
	toast_label = _label(Vector2(340, 110), 22, Color(1, 1, 1))
	toast_label.visible = false
	menu_label = _label(Vector2(390, 280), 40, Color(0.24, 0.95, 1.0))
	menu_label.text = "SEKTOR 7\n\nCLICK OR PRESS ENTER TO START\nGODOT PORT — MILESTONE 1"

func _label(pos: Vector2, size: int, col: Color) -> Label:
	var l := Label.new()
	l.position = pos
	l.add_theme_font_size_override("font_size", size)
	l.add_theme_color_override("font_color", col)
	add_child(l)
	return l

func _process(delta: float) -> void:
	if game == null or game.player == null:
		return
	hp_label.text = "HP %d/%d    LVL %d    ¢ %d    KILLS %d" % [
		game.player.hp, game.player.max_hp, game.player.level,
		game.player.eddies, game.stats["kills"]]
	ammo_label.text = "AMMO %d / %d%s" % [game.weapons.mag, game.weapons.reserve, "  (RELOADING)" if game.weapons.reloading else ""]
	obj_label.text = "OBJ  GODOT PORT MILESTONE 1 — systems parity"
	menu_label.visible = game.state == "menu"
	if toast_t > 0.0:
		toast_t -= delta
		toast_label.visible = toast_t > 0.0

func toast(text: String, col: Color) -> void:
	toast_label.text = text
	toast_label.add_theme_color_override("font_color", col)
	toast_t = 2.2
	toast_label.visible = true
