extends Node3D

var state := "menu"
var time := 0.0
var stats := {"kills": 0, "eddies": 0}
var upgrade_tiers := {}
var city: Node3D
var player: CharacterBody3D
var cam: Camera3D
var hud: CanvasLayer
var enemies: Node
var vehicles: Node
var weapons: Node
var pov: Node
var quests: Node3D
var audio: Node
var menu: CanvasLayer
var materials: Node
var settings := {}

func _ready() -> void:
	var ui_font := FontFile.new()
	ui_font.load_dynamic_font("res://assets/fonts/DejaVuSans-Bold.ttf")
	ThemeDB.fallback_font = ui_font
	_setup_input()
	_load_settings()
	_setup_audio_buses()
	materials = preload("res://src/materials.gd").new()
	add_child(materials)
	materials.setup(self)
	city = preload("res://src/city.gd").new()
	add_child(city)
	city.build(self)
	player = preload("res://src/player.gd").new()
	add_child(player)
	cam = Camera3D.new()
	cam.fov = 75
	add_child(cam)
	player.setup(self, cam)
	enemies = preload("res://src/enemies.gd").new()
	add_child(enemies)
	enemies.setup(self)
	vehicles = preload("res://src/vehicle.gd").new()
	add_child(vehicles)
	vehicles.setup(self)
	weapons = preload("res://src/weapons.gd").new()
	add_child(weapons)
	weapons.setup(self)
	hud = preload("res://src/hud.gd").new()
	add_child(hud)
	hud.setup(self)
	pov = preload("res://src/pov.gd").new()
	add_child(pov)
	pov.setup(self, cam)
	quests = preload("res://src/quests.gd").new()
	add_child(quests)
	quests.setup(self)
	audio = preload("res://src/audio.gd").new()
	add_child(audio)
	audio.setup(self)
	menu = preload("res://src/menu.gd").new()
	add_child(menu)
	menu.setup(self)
	menu.process_mode = Node.PROCESS_MODE_ALWAYS
	menu.open_main()
	_apply_video_settings()

func _setup_input() -> void:
	_key("move_forward", KEY_W)
	_joy_motion("move_forward", JOY_AXIS_LEFT_Y, -1.0)
	_key("move_back", KEY_S)
	_joy_motion("move_back", JOY_AXIS_LEFT_Y, 1.0)
	_key("move_left", KEY_A)
	_joy_motion("move_left", JOY_AXIS_LEFT_X, -1.0)
	_key("move_right", KEY_D)
	_joy_motion("move_right", JOY_AXIS_LEFT_X, 1.0)
	_key("sprint", KEY_SHIFT)
	_joy_btn("sprint", JOY_BUTTON_LEFT_STICK)
	_key("jump", KEY_SPACE)
	_joy_btn("jump", JOY_BUTTON_A)
	_key("fly_up", KEY_SPACE)
	_joy_btn("fly_up", JOY_BUTTON_RIGHT_SHOULDER)
	_key("fly_down", KEY_C)
	_joy_btn("fly_down", JOY_BUTTON_LEFT_SHOULDER)
	_key("interact", KEY_E)
	_joy_btn("interact", JOY_BUTTON_B)
	_key("reload", KEY_R)
	_joy_btn("reload", JOY_BUTTON_X)
	_key("weapon_1", KEY_1)
	_key("weapon_2", KEY_2)
	_joy_btn("weapon_cycle", JOY_BUTTON_Y)
	_key("pov_cycle", KEY_V)
	_mouse("fire", MOUSE_BUTTON_LEFT)
	_joy_motion("fire", JOY_AXIS_TRIGGER_RIGHT, 1.0)
	_joy_motion("look_left", JOY_AXIS_RIGHT_X, -1.0, 0.12)
	_joy_motion("look_right", JOY_AXIS_RIGHT_X, 1.0, 0.12)
	_joy_motion("look_up", JOY_AXIS_RIGHT_Y, -1.0, 0.12)
	_joy_motion("look_down", JOY_AXIS_RIGHT_Y, 1.0, 0.12)

func _key(action: String, code: Key) -> void:
	_ensure(action)
	var ev := InputEventKey.new()
	ev.keycode = code
	InputMap.action_add_event(action, ev)

func _mouse(action: String, btn: MouseButton) -> void:
	_ensure(action)
	var ev := InputEventMouseButton.new()
	ev.button_index = btn
	InputMap.action_add_event(action, ev)

func _joy_motion(action: String, axis: int, value: float, dz := -1.0) -> void:
	_ensure(action, dz)
	var ev := InputEventJoypadMotion.new()
	ev.axis = axis
	ev.axis_value = value
	InputMap.action_add_event(action, ev)

func _joy_btn(action: String, btn: JoyButton) -> void:
	_ensure(action)
	var ev := InputEventJoypadButton.new()
	ev.button_index = btn
	InputMap.action_add_event(action, ev)

func _ensure(action: String, dz := -1.0) -> void:
	if not InputMap.has_action(action):
		if dz >= 0.0:
			InputMap.add_action(action, dz)
		else:
			InputMap.add_action(action)

func _unhandled_input(event: InputEvent) -> void:
	if event is InputEventMouseMotion and Input.mouse_mode == Input.MOUSE_MODE_CAPTURED:
		player.on_look(event.relative.x, event.relative.y)
	elif event is InputEventMouseButton and event.pressed and event.button_index == MOUSE_BUTTON_LEFT:
		if state == "menu":
			start_game()
	elif event is InputEventKey and event.pressed and not event.echo:
		var code: Key = event.keycode
		if code == KEY_ESCAPE:
			if state == "playing":
				pause_game()
			elif state == "paused":
				if not menu.consume_esc():
					resume_game()
			elif state == "shop":
				close_shop()
		elif code == KEY_ENTER:
			if state == "menu":
				start_game()
			elif state == "dead":
				respawn()
			elif state == "won":
				resume_from_win()
			elif state == "shop":
				close_shop()
		elif code == KEY_E and state == "shop":
			close_shop()
		elif event.is_action_pressed("pov_cycle") and state == "playing":
			pov.cycle()

func start_game() -> void:
	if state != "menu":
		return
	menu.close_all()
	state = "playing"
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED
	hud.toast("SEKTOR 7 — GODOT PORT MILESTONE 2", Color(0.24, 0.95, 1.0))
	_begin_first_job()

func _begin_first_job() -> void:
	await get_tree().create_timer(0.9).timeout
	if state == "playing" and quests.idx == -1:
		quests.begin(0)

func pause_game() -> void:
	if state != "playing":
		return
	state = "paused"
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	get_tree().paused = true
	menu.open_pause()

func resume_game() -> void:
	if state != "paused":
		return
	menu.close_all()
	state = "playing"
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED
	get_tree().paused = false

func on_player_death() -> void:
	state = "dead"
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	hud.toast("FLATLINED — press ENTER to respawn", Color(1, 0.28, 0.34))
	audio.sfx("boom")

func respawn() -> void:
	player.reset()
	state = "playing"
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED

func open_shop() -> void:
	if state != "playing":
		return
	state = "shop"
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	hud.open_shop()
	audio.sfx("ui")

func close_shop() -> void:
	if state != "shop":
		return
	hud.close_shop()
	state = "playing"
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED

func win() -> void:
	state = "won"
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	hud.show_win(stats["kills"], stats["eddies"], player.level)
	audio.sfx("level")

func resume_from_win() -> void:
	hud.hide_win()
	state = "playing"
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED

func add_eddies(n: int) -> void:
	player.eddies += n
	stats["eddies"] += n

func on_enemy_killed(kind: String) -> void:
	stats["kills"] += 1
	var xp := 50
	if kind == "drone":
		xp = 40
	elif kind == "boss":
		xp = 300
	player.add_xp(xp)
	quests.on_kill(kind)
	audio.sfx("boom", 0.6)

func _load_settings() -> void:
	settings = {
		"master": 1.0, "music": 1.0, "sfx": 1.0,
		"fov": 75.0, "brightness": 1.0, "window_mode": 0,
		"mouse_sens": 1.0, "stick_sens": 1.0
	}
	var cfg := ConfigFile.new()
	if cfg.load("user://settings.cfg") == OK:
		settings["master"] = float(cfg.get_value("audio", "master", 1.0))
		settings["music"] = float(cfg.get_value("audio", "music", 1.0))
		settings["sfx"] = float(cfg.get_value("audio", "sfx", 1.0))
		settings["fov"] = float(cfg.get_value("video", "fov", 75.0))
		settings["brightness"] = float(cfg.get_value("video", "brightness", 1.0))
		settings["window_mode"] = int(cfg.get_value("video", "window_mode", 0))
		settings["mouse_sens"] = float(cfg.get_value("controls", "mouse_sens", 1.0))
		settings["stick_sens"] = float(cfg.get_value("controls", "stick_sens", 1.0))
		_apply_saved_binds(cfg)

func _apply_saved_binds(cfg: ConfigFile) -> void:
	if cfg.has_section("binds"):
		for action in cfg.get_section_keys("binds"):
			var kc: int = int(cfg.get_value("binds", action, 0))
			if kc > 0 and InputMap.has_action(action):
				_replace_key(action, kc)
	if cfg.has_section("pad_binds"):
		for action in cfg.get_section_keys("pad_binds"):
			var bi: int = int(cfg.get_value("pad_binds", action, -1))
			if bi >= 0 and InputMap.has_action(action):
				_replace_pad(action, bi)

func _replace_key(action: String, kc: int) -> void:
	for ev in InputMap.action_get_events(action):
		if ev is InputEventKey:
			InputMap.action_erase_event(action, ev)
	var ev := InputEventKey.new()
	ev.keycode = kc
	InputMap.action_add_event(action, ev)

func _replace_pad(action: String, btn: int) -> void:
	for ev in InputMap.action_get_events(action):
		if ev is InputEventJoypadButton:
			InputMap.action_erase_event(action, ev)
	var ev := InputEventJoypadButton.new()
	ev.button_index = btn
	InputMap.action_add_event(action, ev)

func save_settings() -> void:
	var cfg := ConfigFile.new()
	cfg.set_value("audio", "master", settings["master"])
	cfg.set_value("audio", "music", settings["music"])
	cfg.set_value("audio", "sfx", settings["sfx"])
	cfg.set_value("video", "fov", settings["fov"])
	cfg.set_value("video", "brightness", settings["brightness"])
	cfg.set_value("video", "window_mode", settings["window_mode"])
	cfg.set_value("controls", "mouse_sens", settings["mouse_sens"])
	cfg.set_value("controls", "stick_sens", settings["stick_sens"])
	cfg.save("user://settings.cfg")

func _setup_audio_buses() -> void:
	if AudioServer.get_bus_index("SFX") == -1:
		AudioServer.add_bus()
		AudioServer.set_bus_name(AudioServer.bus_count - 1, "SFX")
		AudioServer.set_bus_send(AudioServer.bus_count - 1, "Master")
	if AudioServer.get_bus_index("Music") == -1:
		AudioServer.add_bus()
		AudioServer.set_bus_name(AudioServer.bus_count - 1, "Music")
		AudioServer.set_bus_send(AudioServer.bus_count - 1, "Master")
	for pair in [["Master", "master"], ["SFX", "sfx"], ["Music", "music"]]:
		AudioServer.set_bus_volume_linear(AudioServer.get_bus_index(pair[0]), float(settings[pair[1]]))

func _apply_video_settings() -> void:
	cam.fov = float(settings["fov"])
	if city != null and city.env != null:
		city.env.set("adjustment_brightness", float(settings["brightness"]))
	_apply_window_mode(int(settings["window_mode"]))

func _apply_window_mode(mode: int) -> void:
	if DisplayServer.get_name() == "headless":
		return
	match mode:
		1:
			DisplayServer.window_set_mode(DisplayServer.WINDOW_MODE_FULLSCREEN)
		2:
			DisplayServer.window_set_flag(DisplayServer.WINDOW_FLAG_BORDERLESS, false)
			DisplayServer.window_set_mode(DisplayServer.WINDOW_MODE_WINDOWED)
			DisplayServer.window_set_size(DisplayServer.screen_get_size(DisplayServer.SCREEN_PRIMARY))
			DisplayServer.window_set_position(Vector2i(0, 0))
			DisplayServer.window_set_flag(DisplayServer.WINDOW_FLAG_BORDERLESS, true)
		_:
			DisplayServer.window_set_flag(DisplayServer.WINDOW_FLAG_BORDERLESS, false)
			DisplayServer.window_set_mode(DisplayServer.WINDOW_MODE_WINDOWED)

func return_to_main_menu() -> void:
	save_settings()
	get_tree().paused = false
	if get_tree().current_scene == null:
		state = "menu"
		menu.open_main()
		return
	get_tree().reload_current_scene()

func _physics_process(delta: float) -> void:
	if state == "playing":
		time += delta
		var lx := Input.get_action_strength("look_right") - Input.get_action_strength("look_left")
		var ly := Input.get_action_strength("look_down") - Input.get_action_strength("look_up")
		var lmag := Vector2(lx, ly).length()
		if lmag > 0.001:
			var curved := pow(lmag, 1.7)
			var ss := float(settings.get("stick_sens", 1.0))
			player.on_look(lx / lmag * curved * delta * 1500.0 * ss, ly / lmag * curved * delta * 1200.0 * ss)
		if Input.is_action_just_pressed("interact"):
			if hud.dialogue_active():
				hud.advance_dialogue()
			elif vehicles._nearest_car() == null and player.position.distance_to(city.terminal) < 2.9:
				open_shop()
	elif state == "menu":
		player.rotation.y += delta * 0.04
