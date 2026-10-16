extends Node

var game
var list := []
var pickups := []
var _fx := []
var rng := RandomNumberGenerator.new()

class Merc extends CharacterBody3D:
	var hp := 100.0
	var mgr
	var kind := "merc"
	var aggressive := false
	var state := "patrol"
	var wp := Vector3.ZERO
	var home := Vector3.ZERO
	var speed := 1.7
	var burst := 0
	var burst_max := 3
	var shot_t := 0.0
	var next_burst := 1.5
	var dmg := [5, 8]
	var stuck_t := 0.0
	var taunted := false

	func damage(n: float) -> bool:
		hp -= n
		state = "combat"
		if hp <= 0.0:
			mgr.on_death(self)
			return true
		return false

	func _physics_process(delta: float) -> void:
		if mgr != null:
			mgr.merc_ai(self, delta)

class Drone extends CharacterBody3D:
	var hp := 40.0
	var mgr
	var kind := "drone"
	var anchor := Vector3.ZERO
	var orbit := 0.0
	var fire_t := 1.5

	func damage(n: float) -> bool:
		hp -= n
		if hp <= 0.0:
			mgr.on_death(self)
			return true
		return false

	func _physics_process(delta: float) -> void:
		if mgr != null:
			mgr.drone_ai(self, delta)

func setup(g) -> void:
	game = g
	rng.seed = 77

func spawn_merc(x: float, z: float, aggressive := false) -> Merc:
	return _make_merc(x, z, aggressive, Color(0.24, 0.95, 1.0))

func spawn_boss(x: float, z: float) -> Merc:
	var e := _make_merc(x, z, true, Color(1.0, 0.13, 0.2))
	e.kind = "boss"
	e.hp = 600.0
	e.speed = 3.0
	e.dmg = [6, 9]
	e.burst_max = 5
	e.scale = Vector3(1.35, 1.35, 1.35)
	return e

func _make_merc(x: float, z: float, aggressive: bool, visor: Color) -> Merc:
	var e := Merc.new()
	e.mgr = self
	e.aggressive = aggressive
	e.position = Vector3(x, 0.9, z)
	e.home = e.position
	e.wp = e.position
	var shape := CollisionShape3D.new()
	var cap := CapsuleShape3D.new()
	cap.radius = 0.4
	cap.height = 1.8
	shape.shape = cap
	e.add_child(shape)
	_merc_mesh(e, visor)
	e.add_to_group("enemies")
	add_child(e)
	list.append(e)
	return e

func _merc_mesh(e: Node3D, visor: Color) -> void:
	var torso := MeshInstance3D.new()
	var tm := BoxMesh.new()
	tm.size = Vector3(0.52, 0.62, 0.3)
	torso.mesh = tm
	torso.position = Vector3(0, 0.22, 0)
	var tmat := StandardMaterial3D.new()
	tmat.albedo_color = Color(0.16, 0.14, 0.2)
	torso.material_override = tmat
	e.add_child(torso)
	var head := MeshInstance3D.new()
	var hm := BoxMesh.new()
	hm.size = Vector3(0.24, 0.26, 0.26)
	head.mesh = hm
	head.position = Vector3(0, 0.68, 0)
	var hmat := StandardMaterial3D.new()
	hmat.albedo_color = Color(0.08, 0.09, 0.12)
	head.material_override = hmat
	e.add_child(head)
	var vis := MeshInstance3D.new()
	var vm := BoxMesh.new()
	vm.size = Vector3(0.26, 0.06, 0.06)
	vis.mesh = vm
	vis.position = Vector3(0, 0.7, 0.15)
	var vmat := StandardMaterial3D.new()
	vmat.emission_enabled = true
	vmat.emission = visor
	vmat.emission_energy_multiplier = 2.0
	vis.material_override = vmat
	e.add_child(vis)
	var hip := MeshInstance3D.new()
	var hipm := BoxMesh.new()
	hipm.size = Vector3(0.4, 0.22, 0.24)
	hip.mesh = hipm
	hip.position = Vector3(0, -0.14, 0)
	hip.material_override = tmat
	e.add_child(hip)
	for sx in [-0.11, 0.11]:
		var leg := MeshInstance3D.new()
		var lm := BoxMesh.new()
		lm.size = Vector3(0.15, 0.62, 0.18)
		leg.mesh = lm
		leg.position = Vector3(sx, -0.55, 0)
		leg.material_override = tmat
		e.add_child(leg)
	for sx in [-0.34, 0.34]:
		var arm := MeshInstance3D.new()
		var am2 := BoxMesh.new()
		am2.size = Vector3(0.12, 0.5, 0.14)
		arm.mesh = am2
		arm.position = Vector3(sx, 0.18, 0)
		arm.material_override = tmat
		e.add_child(arm)
	var pads := MeshInstance3D.new()
	var padm := BoxMesh.new()
	padm.size = Vector3(0.52, 0.05, 0.28)
	pads.mesh = padm
	pads.position = Vector3(0, 0.54, 0)
	var padmat := StandardMaterial3D.new()
	padmat.emission_enabled = true
	padmat.emission = visor
	padmat.emission_energy_multiplier = 1.6
	pads.material_override = padmat
	e.add_child(pads)

func spawn_drone(x: float, z: float) -> Drone:
	var d := Drone.new()
	d.mgr = self
	d.position = Vector3(x, 7.0, z)
	d.anchor = d.position
	d.motion_mode = CharacterBody3D.MOTION_MODE_FLOATING
	var cs := CollisionShape3D.new()
	var bs := BoxShape3D.new()
	bs.size = Vector3(0.9, 0.9, 0.9)
	cs.shape = bs
	d.add_child(cs)
	var mi := MeshInstance3D.new()
	var om := CylinderMesh.new()
	om.top_radius = 0.55
	om.bottom_radius = 0.55
	om.height = 0.26
	om.radial_segments = 8
	mi.mesh = om
	var mat := StandardMaterial3D.new()
	mat.albedo_color = Color(0.1, 0.125, 0.17)
	mat.metallic = 0.8
	mat.roughness = 0.35
	mi.material_override = mat
	d.add_child(mi)
	for off in [Vector3(0.55, 0, 0.55), Vector3(-0.55, 0, 0.55), Vector3(0.55, 0, -0.55), Vector3(-0.55, 0, -0.55)]:
		var pod := MeshInstance3D.new()
		var pb := BoxMesh.new()
		pb.size = Vector3(0.26, 0.12, 0.26)
		pod.mesh = pb
		pod.position = off
		pod.material_override = mat
		d.add_child(pod)
		var rotor := MeshInstance3D.new()
		var rb := CylinderMesh.new()
		rb.top_radius = 0.22
		rb.bottom_radius = 0.22
		rb.height = 0.02
		rotor.mesh = rb
		rotor.position = off + Vector3(0, 0.09, 0)
		var rmat := StandardMaterial3D.new()
		rmat.emission_enabled = true
		rmat.emission = Color(0.4, 0.75, 1.0)
		rmat.emission_energy_multiplier = 1.4
		rotor.material_override = rmat
		d.add_child(rotor)
	var ring := MeshInstance3D.new()
	var rg := CylinderMesh.new()
	rg.top_radius = 0.42
	rg.bottom_radius = 0.42
	rg.height = 0.03
	ring.mesh = rg
	ring.position = Vector3(0, -0.15, 0)
	var rgmat := StandardMaterial3D.new()
	rgmat.emission_enabled = true
	rgmat.emission = Color(1.0, 0.2, 0.27)
	rgmat.emission_energy_multiplier = 1.8
	ring.material_override = rgmat
	d.add_child(ring)
	var eye := MeshInstance3D.new()
	var eb := BoxMesh.new()
	eb.size = Vector3(0.2, 0.2, 0.2)
	eye.mesh = eb
	eye.position = Vector3(0, 0, 0.4)
	var emat := StandardMaterial3D.new()
	emat.emission_enabled = true
	emat.emission = Color(1.0, 0.2, 0.27)
	emat.emission_energy_multiplier = 2.2
	eye.material_override = emat
	d.add_child(eye)
	d.add_to_group("enemies")
	add_child(d)
	list.append(d)
	return d

func on_death(e) -> void:
	var kind: String = e.kind
	game.on_enemy_killed(kind)
	list.erase(e)
	_spawn_pickups(e.position, kind)
	_explode(e.position + Vector3(0, 0.8, 0))
	e.queue_free()

func _explode(pos: Vector3) -> void:
	var p := CPUParticles3D.new()
	p.one_shot = true
	p.emitting = true
	p.amount = 42
	p.lifetime = 0.85
	p.explosiveness = 1.0
	p.emission_shape = CPUParticles3D.EMISSION_SHAPE_SPHERE
	p.emission_sphere_radius = 0.35
	p.spread = 180.0
	p.gravity = Vector3(0, -9, 0)
	p.initial_velocity_min = 4.0
	p.initial_velocity_max = 9.0
	var m := BoxMesh.new()
	m.size = Vector3(0.09, 0.09, 0.09)
	var mat := StandardMaterial3D.new()
	mat.emission_enabled = true
	mat.emission = Color(1.0, 0.55, 0.2)
	mat.emission_energy_multiplier = 3.0
	mat.albedo_color = Color(0.1, 0.06, 0.03)
	m.material = mat
	p.mesh = m
	p.position = pos
	add_child(p)
	var light := OmniLight3D.new()
	light.position = pos
	light.light_color = Color(1.0, 0.6, 0.3)
	light.light_energy = 4.0
	light.omni_range = 12.0
	add_child(light)
	_fx.append({"p": p, "light": light, "t": 0.9})

func _spawn_pickups(pos: Vector3, kind: String) -> void:
	var val := 40
	if kind == "boss":
		val = 300
	elif kind == "drone":
		val = 30
	_spawn_pickup(pos, "eddies", val)
	if rng.randf() < 0.35:
		_spawn_pickup(pos + Vector3(1, 0, 0), "ammo", 0)
	if rng.randf() < 0.2:
		_spawn_pickup(pos - Vector3(1, 0, 0), "stim", 0)

func _spawn_pickup(pos: Vector3, kind: String, val: int) -> void:
	var area := Area3D.new()
	var cs := CollisionShape3D.new()
	var ss := SphereShape3D.new()
	ss.radius = 1.4
	cs.shape = ss
	area.add_child(cs)
	var mi := MeshInstance3D.new()
	var om := PrismMesh.new()
	om.size = Vector3(0.4, 0.45, 0.4)
	mi.mesh = om
	var col := Color(0.22, 1.0, 0.53)
	if kind == "ammo":
		col = Color(1.0, 0.84, 0.04)
	elif kind == "stim":
		col = Color(1.0, 0.3, 0.43)
	var mat := StandardMaterial3D.new()
	mat.emission_enabled = true
	mat.emission = col
	mat.emission_energy_multiplier = 2.0
	mi.material_override = mat
	area.add_child(mi)
	area.position = Vector3(pos.x, 0.6, pos.z)
	area.set_meta("kind", kind)
	area.set_meta("val", val)
	area.body_entered.connect(_on_pickup_body.bind(area))
	add_child(area)
	pickups.append({"node": area, "ph": rng.randf() * 9.0})

func _on_pickup_body(body: Node3D, area: Area3D) -> void:
	if body != game.player or game.state != "playing":
		return
	var kind: String = area.get_meta("kind")
	var val := int(area.get_meta("val"))
	if kind == "eddies":
		game.add_eddies(val)
		toast_g("+¢%d" % val, Color(0.22, 1.0, 0.53))
	elif kind == "ammo":
		game.weapons.reserves[0] += 24
		game.weapons.reserves[1] += 64
		toast_g("AMMO +24 / +64", Color(1.0, 0.84, 0.04))
	else:
		game.player.heal(30)
		toast_g("STIM +30 HP", Color(1.0, 0.3, 0.43))
	game.audio.sfx("pickup")
	pickups = pickups.filter(func(pk): return pk["node"] != area)
	area.queue_free()

func toast_g(text: String, col: Color) -> void:
	game.hud.toast(text, col)

func _physics_process(delta: float) -> void:
	for f in _fx.duplicate():
		f["t"] = float(f["t"]) - delta
		var fl: OmniLight3D = f["light"]
		fl.light_energy = 4.0 * maxf(0.0, float(f["t"]) / 0.9)
		if float(f["t"]) <= 0.0:
			f["p"].queue_free()
			fl.queue_free()
			_fx.erase(f)
	if game == null or game.state != "playing":
		return
	for pk in pickups:
		pk["ph"] = float(pk["ph"]) + delta * 3.0
		var n: Area3D = pk["node"]
		n.position.y = 0.6 + sin(float(pk["ph"])) * 0.18
		n.rotate_y(delta * 2.0)

func _los_clear(e: CharacterBody3D, p: CharacterBody3D) -> bool:
	var from := e.position + Vector3(0, 0.65, 0)
	var to := p.position + Vector3(0, 1.6, 0)
	var q := PhysicsRayQueryParameters3D.create(from, to)
	q.exclude = [e.get_rid(), p.get_rid()]
	return game.get_world_3d().direct_space_state.intersect_ray(q).is_empty()

func _merc_fire(e: Merc, dist: float) -> void:
	var acc := 0.48 * clampf(1.25 - dist / 50.0, 0.3, 1.0)
	if rng.randf() < acc:
		game.player.damage(rng.randi_range(e.dmg[0], e.dmg[1]))
		game.audio.sfx("hurt", 0.5)

func merc_ai(e: Merc, delta: float) -> void:
	if game == null or game.state != "playing":
		return
	var p: CharacterBody3D = game.player
	var dist := e.position.distance_to(p.position)
	var los := _los_clear(e, p)
	if dist < 0.5:
		dist = 0.5
	var seen := (e.aggressive and dist < 60.0 and los) or (dist < 38.0 and los)
	if seen and e.state == "patrol":
		e.state = "combat"
	if e.kind == "boss" and not e.taunted and dist < 30.0:
		e.taunted = true
		game.hud.dialogue("RAZORTAIL", ["The plaza rat from Kaneda. Vex sent you?", "I've eaten better with my morning synth-café."])
	match e.state:
		"patrol":
			var d := e.position.distance_to(e.wp)
			if d < 0.5 or e.stuck_t > 1.5:
				e.wp = e.home + Vector3(rng.randf_range(-8, 8), 0, rng.randf_range(-8, 8))
				e.stuck_t = 0.0
			else:
				var dir := e.wp - e.position
				dir.y = 0.0
				dir = dir.normalized()
				e.velocity = dir * e.speed * 0.8
				e.move_and_slide()
				if e.is_on_wall():
					e.stuck_t += delta
				else:
					e.stuck_t = 0.0
				e.rotation.y = atan2(dir.x, dir.z)
		"combat":
			var dir2 := p.position - e.position
			dir2.y = 0.0
			dir2 = dir2.normalized()
			e.rotation.y = lerp_angle(e.rotation.y, atan2(dir2.x, dir2.z), minf(1.0, delta * 6.0))
			var move := Vector3.ZERO
			if dist > 16.0:
				move = dir2 * e.speed
			elif dist < 8.0:
				move = -dir2 * e.speed * 0.8
			else:
				var perp := Vector3(-dir2.z, 0, dir2.x)
				move = perp * sin(game.time * 1.1) * e.speed * 0.8
			e.velocity = move
			e.move_and_slide()
			if los and dist < 46.0:
				e.next_burst -= delta
				if e.next_burst <= 0.0 and e.burst <= 0:
					e.burst = e.burst_max
					e.shot_t = 0.0
					e.next_burst = rng.randf_range(1.4, 2.3)
				if e.burst > 0:
					e.shot_t -= delta
					if e.shot_t <= 0.0:
						_merc_fire(e, dist)
						e.burst -= 1
						e.shot_t = 0.13

func drone_ai(d: Drone, delta: float) -> void:
	if game == null or game.state != "playing":
		return
	var p: CharacterBody3D = game.player
	var dist := d.position.distance_to(p.position)
	d.orbit += delta * 0.55
	var to := Vector3(
		p.position.x + sin(d.orbit) * 13.0,
		6.8 + sin(game.time * 0.9) * 0.9,
		p.position.z + cos(d.orbit) * 13.0)
	var spd := 6.5 if dist < 55.0 else 3.0
	var dirv := to - d.position
	if dirv.length() > 0.05:
		d.velocity = dirv.normalized() * spd
		d.move_and_slide()
	d.look_at(Vector3(p.position.x, 1.6, p.position.z), Vector3.UP)
	if dist < 42.0:
		d.fire_t -= delta
		if d.fire_t <= 0.0:
			d.fire_t = rng.randf_range(1.3, 1.8)
			if rng.randf() < 0.5 * clampf(1.2 - dist / 50.0, 0.3, 1.0):
				game.player.damage(4)
				game.audio.sfx("hurt", 0.4)
