extends Node3D

var game
var cars := []
var vels := {}
var active: CharacterBody3D = null
var driving := false
var _t := 0.0
var _lights := {}
var _underglows := {}

const ACC := 18.0
const DRAG := 1.1
const MAXS := 26.0

func setup(g) -> void:
	game = g
	var spots := [
		{"x": 2.5, "z": 138.0, "name": "QUADRA JINXIE", "body": Color(0.48, 0.12, 0.63), "glow": Color(1.0, 0.16, 0.43)},
		{"x": 52.0, "z": 25.0, "name": "RAIJIN-8", "body": Color(0.08, 0.25, 0.37), "glow": Color(0.24, 0.95, 1.0)},
		{"x": 25.0, "z": -46.0, "name": "VOLGAR RAPTOR", "body": Color(0.23, 0.08, 0.08), "glow": Color(1.0, 0.84, 0.04)},
		{"x": 100.0, "z": -125.0, "name": "MOTO-GHOST", "body": Color(0.06, 0.21, 0.16), "glow": Color(0.22, 1.0, 0.53)}
	]
	for s in spots:
		var car := CharacterBody3D.new()
		car.motion_mode = CharacterBody3D.MOTION_MODE_FLOATING
		car.position = Vector3(s["x"], 0.7, s["z"])
		var shape := CollisionShape3D.new()
		var box := BoxShape3D.new()
		box.size = Vector3(1.9, 1.2, 4.4)
		shape.shape = box
		shape.position = Vector3(0, 0.6, 0)
		car.add_child(shape)
		_car_mesh(car, s["body"], s["glow"])
		car.set_meta("name", s["name"])
		add_child(car)
		cars.append(car)
		vels[car] = Vector3.ZERO

func _car_mesh(car: Node3D, body_col: Color, glow: Color) -> void:
	var hull := MeshInstance3D.new()
	var hm := BoxMesh.new()
	hm.size = Vector3(1.9, 0.5, 4.4)
	hull.mesh = hm
	hull.position = Vector3(0, 0.55, 0)
	var hmat := StandardMaterial3D.new()
	hmat.albedo_color = body_col
	hmat.metallic = 0.8
	hmat.roughness = 0.3
	hmat.clearcoat_enabled = true
	hmat.clearcoat = 0.7
	hmat.clearcoat_roughness = 0.15
	hull.material_override = hmat
	car.add_child(hull)
	for step in [
		{"size": Vector3(1.6, 0.3, 1.0), "pos": Vector3(0, 0.38, 1.85)},
		{"size": Vector3(1.2, 0.2, 0.5), "pos": Vector3(0, 0.28, 2.15)}
	]:
		var nose := MeshInstance3D.new()
		var nm := BoxMesh.new()
		nm.size = step["size"]
		nose.mesh = nm
		nose.position = step["pos"]
		nose.material_override = hmat
		car.add_child(nose)
	var cabin := MeshInstance3D.new()
	var cm := BoxMesh.new()
	cm.size = Vector3(1.68, 0.52, 2.1)
	cabin.mesh = cm
	cabin.position = Vector3(0, 1.05, -0.25)
	var cmat := StandardMaterial3D.new()
	cmat.albedo_color = Color(0.05, 0.09, 0.14, 0.55)
	cmat.metallic = 0.9
	cmat.roughness = 0.08
	cmat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	cabin.material_override = cmat
	car.add_child(cabin)
	_glow(car, Vector3(0, 0.62, 2.22), Vector3(1.7, 0.1, 0.08), Color(0.87, 0.96, 1.0))
	_glow(car, Vector3(0, 0.68, -2.22), Vector3(1.8, 0.12, 0.08), Color(1.0, 0.13, 0.2))
	var under := _glow(car, Vector3(0, 0.1, 0), Vector3(1.6, 0.05, 3.6), glow)
	_underglows[car] = under
	for pz in [1.4, -1.4]:
		_glow(car, Vector3(0, 0.04, pz), Vector3(1.4, 0.06, 0.6), glow)
	var lights := []
	for sx in [-0.55, 0.55]:
		var spot := SpotLight3D.new()
		spot.position = Vector3(sx, 0.72, 2.05)
		spot.rotation = Vector3(deg_to_rad(-6), PI, 0)
		spot.set("spot_range", 26.0)
		spot.set("spot_angle", 24.0)
		spot.set("light_energy", 3.5)
		spot.set("light_color", Color(0.9, 0.95, 1.0))
		spot.visible = false
		car.add_child(spot)
		lights.append(spot)
	_lights[car] = lights
	for dx in [-0.6, 0.6]:
		var disc := MeshInstance3D.new()
		var qm := QuadMesh.new()
		qm.size = Vector2(1.5, 2.4)
		var dmat := StandardMaterial3D.new()
		dmat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
		dmat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
		dmat.cull_mode = BaseMaterial3D.CULL_DISABLED
		dmat.albedo_color = Color(glow.r, glow.g, glow.b, 0.16)
		dmat.emission_enabled = true
		dmat.emission = glow
		dmat.emission_energy_multiplier = 1.2
		qm.material = dmat
		disc.mesh = qm
		disc.rotation.x = -PI / 2
		disc.position = Vector3(dx, 0.06, 0)
		car.add_child(disc)

func _glow(parent: Node3D, pos: Vector3, size: Vector3, col: Color) -> StandardMaterial3D:
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = size
	mi.mesh = bm
	mi.position = pos
	var mat := StandardMaterial3D.new()
	mat.emission_enabled = true
	mat.emission = col
	mat.emission_energy_multiplier = 2.2
	mat.albedo_color = Color(0.03, 0.03, 0.05)
	mi.material_override = mat
	parent.add_child(mi)
	return mat

func _nearest_car() -> CharacterBody3D:
	var p: Vector3 = game.player.position
	var best: CharacterBody3D = null
	var bd := 3.4
	for c in cars:
		var d: float = c.position.distance_to(p)
		if d < bd:
			bd = d
			best = c
	return best

func enter_car(car: CharacterBody3D) -> void:
	if driving:
		return
	driving = true
	active = car
	game.player.in_car = true
	game.player.set_collisions_enabled(false)
	game.hud.toast("THRUSTERS ONLINE — " + str(car.get_meta("name")), Color(0.24, 0.95, 1.0))
	game.audio.engine_on()
	game.audio.sfx("ui")
	if _lights.has(car):
		for l in _lights[car]:
			l.visible = true

func exit_car() -> void:
	if not driving:
		return
	var car := active
	var side: Vector3 = car.transform.basis.x
	game.player.position = car.position + side * 2.3 + Vector3(0, -0.35, 0)
	game.player.position.y = maxf(0.1, game.player.position.y)
	game.player.velocity = Vector3.ZERO
	game.player.in_car = false
	game.player.set_collisions_enabled(true)
	if _lights.has(car):
		for l in _lights[car]:
			l.visible = false
	driving = false
	active = null
	game.audio.engine_off()

func _physics_process(delta: float) -> void:
	if game == null or game.state != "playing":
		return
	_t += delta
	if Input.is_action_just_pressed("interact") and not game.hud.dialogue_active():
		if driving:
			exit_car()
			return
		var near := _nearest_car()
		if near != null:
			enter_car(near)
			return
	if not driving:
		return
	var car: CharacterBody3D = active
	var vel: Vector3 = vels[car]
	var ix := 0.0
	var iz := 0.0
	var vert := 0.0
	if Input.is_action_pressed("move_forward"): iz += 1.0
	if Input.is_action_pressed("move_back"): iz -= 1.0
	if Input.is_action_pressed("move_right"): ix += 1.0
	if Input.is_action_pressed("move_left"): ix -= 1.0
	if Input.is_action_pressed("fly_up"): vert += 1.0
	if Input.is_action_pressed("fly_down"): vert -= 1.0
	var fwd: Vector3 = -game.player.head.global_transform.basis.z
	var right: Vector3 = game.player.global_transform.basis.x
	var acc := Vector3.ZERO
	if iz != 0.0:
		acc += fwd * iz
	if ix != 0.0:
		acc += right * ix
	acc.y += vert
	if acc.length() > 0.001:
		vel += acc.normalized() * ACC * delta
	vel *= exp(-DRAG * delta)
	if vel.length() > MAXS:
		vel = vel.normalized() * MAXS
	if car.position.y < 0.9 and vert <= 0.0:
		vel.y += (0.9 - car.position.y) * 6.0 * delta
	car.velocity = vel
	car.move_and_slide()
	for i in car.get_slide_collision_count():
		var col := car.get_slide_collision(i)
		var n := col.get_normal()
		if vel.dot(n) < -8.0:
			game.hud.toast("IMPACT", Color(1.0, 0.4, 0.4))
			game.audio.sfx("boom", 0.6)
		vel = vel.slide(n)
	vels[car] = vel
	car.position.y = clampf(car.position.y, 0.55, 95.0)
	car.position.x = clampf(car.position.x, -196.0, 196.0)
	car.position.z = clampf(car.position.z, -196.0, 196.0)
	var hs := Vector2(vel.x, vel.z).length()
	if hs > 2.0:
		car.rotation.y = lerp_angle(car.rotation.y, atan2(vel.x, vel.z), minf(1.0, delta * 4.0))
	game.player.position = car.position + car.transform.basis.x * -0.45 + car.transform.basis.z * 0.1 + Vector3(0, -0.55, 0)
	if _underglows.has(car):
		_underglows[car].emission_energy_multiplier = 2.2 + 0.7 * sin(_t * 9.0)
	game.audio.engine_speed(vel.length() / MAXS)
