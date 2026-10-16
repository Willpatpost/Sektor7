extends SceneTree

var passes := 0
var fails := 0
var _done := false

func ok(cond: bool, msg: String) -> void:
	if cond:
		passes += 1
		print("  OK  " + msg)
	else:
		fails += 1
		print("FAIL  " + msg)

func _initialize() -> void:
	call_deferred("_run")
	call_deferred("_watchdog")

func _watchdog() -> void:
	for i in 7000:
		if _done:
			return
		await physics_frame
	if not _done:
		print("FAIL  smoke watchdog timeout")
		quit(2)

func _press_key(code: Key) -> void:
	var k := InputEventKey.new()
	k.keycode = code
	k.pressed = true
	Input.parse_input_event(k)
	await physics_frame
	var k2 := InputEventKey.new()
	k2.keycode = code
	k2.pressed = false
	Input.parse_input_event(k2)

func _run() -> void:
	var cfg_path := ProjectSettings.globalize_path("user://settings.cfg")
	if FileAccess.file_exists(cfg_path):
		DirAccess.remove_absolute(cfg_path)
	var game: Node3D = load("res://main.tscn").instantiate()
	root.add_child(game)
	for i in 12:
		await process_frame
	if game.get("city") == null or game.get("player") == null:
		print("FAIL  main scene failed to initialize (script parse error?)")
		_done = true
		quit(1)
		return

	var city: Node3D = game.city
	ok(city.colliders.size() > 80, "city built (%d colliders)" % city.colliders.size())
	ok(game.materials.set_count() >= 3, "PBR texture sets loaded (%d/4)" % game.materials.set_count())
	ok(game.props.placed_count() > 300, "density pass scattered props (%d instances)" % game.props.placed_count())
	var player: CharacterBody3D = game.player
	ok(player.position.distance_to(city.pois.spawn) < 1.5, "player spawns at POI")

	game.state = "playing"

	var z0 := player.position.z
	Input.action_press("move_forward")
	for i in 180:
		await physics_frame
	Input.action_release("move_forward")
	ok(z0 - player.position.z > 2.0, "player walks forward (%.1fm)" % (z0 - player.position.z))
	ok(player.is_on_floor(), "player stays on floor")

	var y0 := player.position.y
	await _press_key(KEY_SPACE)
	var peak := y0
	for i in 40:
		await physics_frame
		peak = maxf(peak, player.position.y)
	ok(peak > y0 + 0.3, "jump arcs upward (%.2fm)" % (peak - y0))

	var enemies: Node = game.enemies
	var e = enemies.spawn_merc(player.position.x, player.position.z - 12.0, true)
	for i in 240:
		await physics_frame
	ok(e.state == "combat", "merc enters combat")
	ok(e.hp < 100.0 or player.hp < 100, "combat exchanges damage (merc %.0f hp, player %d hp)" % [e.hp, player.hp])

	var weapons: Node = game.weapons
	e.position = Vector3(player.position.x, 0.9, player.position.z - 12.0)
	e.velocity = Vector3.ZERO
	var hp0: float = e.hp
	weapons.mags[0] = 12
	weapons.current = 0
	weapons.cooldown = 0.0
	weapons.reloading = false
	var fbtn := InputEventMouseButton.new()
	fbtn.button_index = MOUSE_BUTTON_LEFT
	fbtn.pressed = true
	Input.parse_input_event(fbtn)
	await physics_frame
	var frel := InputEventMouseButton.new()
	frel.button_index = MOUSE_BUTTON_LEFT
	frel.pressed = false
	Input.parse_input_event(frel)
	await physics_frame
	ok(e.hp < hp0, "hitscan damages merc (%.0f -> %.0f)" % [hp0, e.hp])

	var vehicles: Node = game.vehicles
	var car: CharacterBody3D = vehicles.cars[0]
	player.position = car.position + Vector3(1.5, 0, 0)
	player.velocity = Vector3.ZERO
	for i in 6:
		await process_frame
	vehicles.enter_car(car)
	ok(vehicles.driving and player.in_car, "enter hovercar")
	var start := car.position
	Input.action_press("move_forward")
	for i in 240:
		await physics_frame
	Input.action_release("move_forward")
	ok(car.position.distance_to(start) > 2.0, "hovercar flies (%.1fm)" % car.position.distance_to(start))
	Input.action_press("fly_up")
	for i in 120:
		await physics_frame
	Input.action_release("fly_up")
	ok(car.position.y > 1.5, "hovercar climbs (%.1fm altitude)" % car.position.y)
	vehicles.exit_car()
	ok(not player.in_car and not vehicles.driving, "exit hovercar")
	ok(player.position.y >= 0.0, "player exits above ground")

	player.hp = 100000
	var quests: Node3D = game.quests
	quests.begin(0)
	await physics_frame
	ok(game.hud.dialogue_active(), "intro dialogue opens")
	var guard := 0
	while game.hud.dialogue_active() and guard < 60:
		await _press_key(KEY_E)
		guard += 1
	ok(not game.hud.dialogue_active(), "dialogue advances and closes")

	guard = 0
	while quests.idx == 0 and guard < 500:
		for en in enemies.list.duplicate():
			if en.kind == "merc":
				en.damage(99999.0)
		if game.hud.dialogue_active():
			await _press_key(KEY_E)
		await physics_frame
		guard += 1
	ok(quests.idx == 1, "job 1 completes on kills (idx %d)" % quests.idx)
	while game.hud.dialogue_active():
		await _press_key(KEY_E)

	var shard = quests.get("shard")
	ok(shard != null, "job 2 spawns the data shard")
	if shard != null:
		player.position = Vector3(shard.position.x, 0, shard.position.z)
		guard = 0
		while quests.idx == 1 and guard < 400:
			if game.hud.dialogue_active():
				await _press_key(KEY_E)
			await physics_frame
			guard += 1
		ok(quests.idx == 2, "shard collection advances job 2 (idx %d)" % quests.idx)
	while game.hud.dialogue_active():
		await _press_key(KEY_E)

	guard = 0
	while quests.idx == 2 and guard < 500:
		for en in enemies.list.duplicate():
			if en.kind == "drone":
				en.damage(99999.0)
		if game.hud.dialogue_active():
			await _press_key(KEY_E)
		await physics_frame
		guard += 1
	ok(quests.idx == 3, "job 3 completes on drone kills (idx %d)" % quests.idx)
	while game.hud.dialogue_active():
		await _press_key(KEY_E)

	guard = 0
	while quests.idx == 3 and not quests.all_done and guard < 500:
		for en in enemies.list.duplicate():
			if en.kind == "boss" or en.kind == "merc":
				en.damage(999999.0)
		if game.hud.dialogue_active():
			await _press_key(KEY_E)
		await physics_frame
		guard += 1
	ok(quests.all_done, "boss kill completes the chain")
	ok(game.state == "won", "win state reached")
	await _press_key(KEY_ENTER)
	ok(game.state == "playing", "ENTER resumes free roam after win")

	player.position = game.city.terminal + Vector3(0, 0, 1.5)
	player.velocity = Vector3.ZERO
	for i in 6:
		await process_frame
	await _press_key(KEY_E)
	await physics_frame
	ok(game.state == "shop", "E at terminal opens the shop")
	var edd0: int = player.eddies
	var hpmax0: int = player.max_hp
	game.hud.buy("plating")
	ok(player.max_hp == hpmax0 + 25 and player.eddies < edd0, "upgrade purchase applies (max hp %d -> %d)" % [hpmax0, player.max_hp])
	game.close_shop()
	await physics_frame
	ok(game.state == "playing", "shop closes back to playing")

	var cam: Camera3D = game.cam
	var cam_dist: Callable = func() -> float:
		return cam.position.distance_to(player.head.global_position)
	for i in 60:
		await physics_frame
	var pv0: float = cam_dist.call()
	ok(pv0 < 0.8, "starts first person (%.2fm)" % pv0)
	var pov_specs := [
		{"label": "behind-close", "lo": 1.6, "hi": 3.6},
		{"label": "behind-far", "lo": 4.2, "hi": 6.9},
		{"label": "front-close", "lo": 1.4, "hi": 3.6},
		{"label": "front-far", "lo": 3.8, "hi": 6.5}
	]
	for spec in pov_specs:
		await _press_key(KEY_V)
		for i in 40:
			await physics_frame
		var dd: float = cam_dist.call()
		ok(dd > float(spec["lo"]) and dd < float(spec["hi"]), "POV %s offset %.2fm" % [spec["label"], dd])
		if spec["label"] == "behind-close":
			ok(player.body.visible, "body visible in third person")
	await _press_key(KEY_V)
	for i in 40:
		await physics_frame
	ok(cam_dist.call() < 0.8, "POV cycles back to first person")
	ok(not player.body.visible, "body hidden in first person")

	var edd1: int = player.eddies
	enemies._spawn_pickup(player.position + Vector3(0, 0.6, 3), "eddies", 55)
	await physics_frame
	player.position = Vector3(player.position.x, 0, player.position.z + 3)
	for i in 12:
		await physics_frame
	ok(player.eddies > edd1, "eddies pickup collected (+%d)" % (player.eddies - edd1))

	var look_ok := false
	for evx in InputMap.action_get_events("look_right"):
		var jm := evx as InputEventJoypadMotion
		if jm != null and jm.axis == JOY_AXIS_RIGHT_X and jm.axis_value > 0.5:
			look_ok = true
	ok(look_ok, "right stick bound to look_right")
	var yaw0 := player.rotation.y
	Input.action_press("look_right", 1.0)
	for i in 30:
		await physics_frame
	Input.action_release("look_right")
	ok(yaw0 - player.rotation.y > 0.05, "gamepad look yaws right (%.2f rad)" % (yaw0 - player.rotation.y))
	var pitch0: float = player.head.rotation.x
	Input.action_press("look_down", 1.0)
	for i in 30:
		await physics_frame
	Input.action_release("look_down")
	ok(pitch0 - player.head.rotation.x > 0.02, "gamepad look pitches down (%.2f rad)" % (pitch0 - player.head.rotation.x))
	ok(game.weapons.vm.visible, "weapon viewmodel visible in first person")
	await _press_key(KEY_V)
	for i in 40:
		await physics_frame
	ok(not game.weapons.vm.visible, "viewmodel hidden in third person")
	game.pov.idx = 0
	for i in 40:
		await physics_frame
	ok(game.weapons.vm.visible, "viewmodel returns in first person")

	await _press_key(KEY_V)
	ok(game.hud.pov_label.visible, "POV switch shows bottom-center toast")
	ok(game.hud.hp_label.anchor_top == 1.0, "vitals anchored bottom-left")
	ok(game.hud.obj_panel.anchor_left == 1.0, "objective anchored top-right")

	await _press_key(KEY_ESCAPE)
	ok(game.state == "paused" and game.menu.pause_root.visible, "ESC opens pause menu")
	ok(paused, "pause freezes the scene tree")
	var tnode: Node3D = game.city._traffic[0]["node"]
	var tpos := tnode.position
	for i in 20:
		await physics_frame
	ok(tpos == tnode.position, "hover traffic frozen while paused")
	await _press_key(KEY_ESCAPE)
	ok(game.state == "playing" and not game.menu.pause_root.visible, "ESC resumes")
	ok(not paused, "resume unfreezes the scene tree")
	game.menu.set_fov(90.0)
	ok(absf(game.cam.fov - 90.0) < 0.5, "fov setting applies live")
	game.menu.set_brightness(1.3)
	ok(absf(float(game.city.env.get("adjustment_brightness")) - 1.3) < 0.05, "brightness applies live")
	game.menu.set_volume("sfx", 0.5)
	ok(absf(AudioServer.get_bus_volume_db(AudioServer.get_bus_index("SFX")) - linear_to_db(0.5)) < 0.1, "sfx slider routes to audio bus")
	ok(FileAccess.file_exists(cfg_path), "settings persist to file")
	game.menu._commit_remap("interact", KEY_F)
	var has_f := false
	for ev in InputMap.action_get_events("interact"):
		if ev is InputEventKey and ev.keycode == KEY_F:
			has_f = true
	ok(has_f, "key remap applies to InputMap")
	game.menu.open_settings_from("pause")
	ok(game.menu.settings_root.visible, "settings panel opens")
	await _press_key(KEY_ESCAPE)
	ok(game.state == "paused" and game.menu.pause_root.visible and not game.menu.settings_root.visible, "ESC in settings backs out to pause")
	await _press_key(KEY_ESCAPE)
	ok(game.state == "playing", "ESC resumes after settings")
	ok(game.menu.btn_main_menu != null, "main menu return button present")
	DirAccess.remove_absolute(cfg_path)

	print("\n%d passed, %d failed" % [passes, fails])
	_done = true
	game.free()
	quit(1 if fails > 0 else 0)
