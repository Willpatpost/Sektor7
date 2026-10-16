extends Node

var game
var list := []
var rng := RandomNumberGenerator.new()

class Merc extends CharacterBody3D:
	var hp := 100.0
	var mgr
	var aggressive := false
	var state := "patrol"
	var wp := Vector3.ZERO
	var home := Vector3.ZERO
	var speed := 1.7
	var burst := 0
	var shot_t := 0.0
	var next_burst := 1.5
	var dmg := [5, 8]
	var stuck_t := 0.0

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

func setup(g: Node3D) -> void:
	game = g
	rng.seed = 77

func spawn_merc(x: float, z: float, aggressive := false) -> Merc:
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
	_merc_mesh(e)
	e.add_to_group("enemies")
	add_child(e)
	list.append(e)
	return e

func _merc_mesh(e: Node3D) -> void:
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
	var visor := MeshInstance3D.new()
	var vm := BoxMesh.new()
	vm.size = Vector3(0.26, 0.06, 0.06)
	visor.mesh = vm
	visor.position = Vector3(0, 0.7, 0.15)
	var vmat := StandardMaterial3D.new()
	vmat.emission_enabled = true
	vmat.emission = Color(0.24, 0.95, 1.0)
	vmat.emission_energy_multiplier = 2.0
	visor.material_override = vmat
	e.add_child(visor)

func on_death(e: Merc) -> void:
	game.on_enemy_killed(e)
	list.erase(e)
	e.queue_free()

func _los_clear(e: Merc, p: CharacterBody3D) -> bool:
	var from := e.position + Vector3(0, 0.65, 0)
	var to := p.position + Vector3(0, 1.6, 0)
	var q := PhysicsRayQueryParameters3D.create(from, to)
	q.exclude = [e.get_rid(), p.get_rid()]
	return game.get_world_3d().direct_space_state.intersect_ray(q).is_empty()

func _merc_fire(e: Merc, dist: float) -> void:
	var acc := 0.48 * clampf(1.25 - dist / 50.0, 0.3, 1.0)
	if rng.randf() < acc:
		game.player.damage(rng.randi_range(e.dmg[0], e.dmg[1]))

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
	match e.state:
		"patrol":
			var d := e.position.distance_to(e.wp)
			if d < 0.5 or e.stuck_t > 1.5:
				e.wp = e.home + Vector3(rng.randf_range(-8, 8), 0, rng.randf_range(-8, 8))
				e.stuck_t = 0.0
			else:
				var dir := (e.wp - e.position)
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
					e.burst = 3
					e.shot_t = 0.0
					e.next_burst = rng.randf_range(1.4, 2.3)
				if e.burst > 0:
					e.shot_t -= delta
					if e.shot_t <= 0.0:
						_merc_fire(e, dist)
						e.burst -= 1
						e.shot_t = 0.13
