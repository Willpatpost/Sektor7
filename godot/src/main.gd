extends Node3D

var state := "menu"
var time := 0.0
var stats := {"kills": 0, "eddies": 0}
var city: Node3D
var player: CharacterBody3D
var hud: CanvasLayer
var enemies: Node
var vehicles: Node
var weapons: Node

func _ready() -> void:
	_setup_input()
	city = preload("res://src/city.gd").new()
	add_child(city)
	city.build(self)
	player = preload("res://src/player.gd").new()
	add_child(player)
	player.setup(self)
	hud = preload("res://src/hud.gd").new()
	add_child(hud)
	hud.setup(self)
	enemies = preload("res://src/enemies.gd").new()
	add_child(enemies)
	enemies.setup(self)
	vehicles = preload("res://src/vehicle.gd").new()
	add_child(vehicles)
	vehicles.setup(self)
	weapons = preload("res://src/weapons.gd").new()
	add_child(weapons)
	weapons.setup(self)

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
	_mouse("fire", MOUSE_BUTTON_LEFT)
	_joy_motion("fire", JOY_AXIS_TRIGGER_RIGHT, 1.0)

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

func _joy_motion(action: String, axis: int, value: float) -> void:
	_ensure(action)
	var ev := InputEventJoypadMotion.new()
	ev.axis = axis
	ev.axis_value = value
	InputMap.action_add_event(action, ev)

func _joy_btn(action: String, btn: JoyButton) -> void:
	_ensure(action)
	var ev := InputEventJoypadButton.new()
	ev.button_index = btn
	InputMap.action_add_event(action, ev)

func _ensure(action: String) -> void:
	if not InputMap.has_action(action):
		InputMap.add_action(action)

func _unhandled_input(event: InputEvent) -> void:
	if event is InputEventMouseMotion and Input.mouse_mode == Input.MOUSE_MODE_CAPTURED:
		player.on_look(event.relative.x, event.relative.y)
	elif event is InputEventMouseButton and event.pressed and event.button_index == MOUSE_BUTTON_LEFT:
		if state == "menu":
			start_game()
	elif event is InputEventKey and event.pressed and not event.echo:
		if event.keycode == KEY_ESCAPE:
			if state == "playing":
				pause_game()
			elif state == "paused":
				resume_game()
		elif event.keycode == KEY_ENTER:
			if state == "menu":
				start_game()
			elif state == "dead":
				respawn()

func start_game() -> void:
	state = "playing"
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED
	hud.toast("SEKTOR 7 — GODOT PORT MILESTONE 1", Color(0.24, 0.95, 1.0))

func pause_game() -> void:
	if state != "playing":
		return
	state = "paused"
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE

func resume_game() -> void:
	state = "playing"
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED

func on_player_death() -> void:
	state = "dead"
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	hud.toast("FLATLINED — press ENTER to respawn", Color(1, 0.28, 0.34))

func respawn() -> void:
	player.reset()
	state = "playing"
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED

func add_eddies(n: int) -> void:
	player.eddies += n
	stats["eddies"] += n

func on_enemy_killed(_e: Node) -> void:
	stats["kills"] += 1
	player.add_xp(50)
	add_eddies(40)

func _physics_process(delta: float) -> void:
	if state == "playing":
		time += delta
