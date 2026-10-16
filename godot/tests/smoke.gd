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
	for i in 3600:
		if _done:
			return
		await physics_frame
	if not _done:
		print("FAIL  smoke watchdog timeout")
		quit(2)

func _run() -> void:
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
	var jkey := InputEventKey.new()
	jkey.keycode = KEY_SPACE
	jkey.pressed = true
	Input.parse_input_event(jkey)
	await physics_frame
	var jrel := InputEventKey.new()
	jrel.keycode = KEY_SPACE
	jrel.pressed = false
	Input.parse_input_event(jrel)
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
	weapons.mag = 12
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

	print("\n%d passed, %d failed" % [passes, fails])
	_done = true
	game.free()
	quit(1 if fails > 0 else 0)
