extends Node

var game
var defs := [
	{"name": "KURO PISTOL", "dmg": 34.0, "rate": 0.16, "mag": 12, "reload": 1.05, "auto": false},
	{"name": "WASP SMG", "dmg": 12.0, "rate": 0.083, "mag": 32, "reload": 1.5, "auto": true}
]
var mags := [12, 0]
var reserves := [48, 96]
var unlocked := [true, false]
var current := 0
var cooldown := 0.0
var reloading := false
var reload_t := 0.0
var _flashes := []
var _tracers := []

func setup(g) -> void:
	game = g

func _def() -> Dictionary:
	return defs[current]

func unlock_smg() -> void:
	if unlocked[1]:
		return
	unlocked[1] = true
	mags[1] = int(defs[1]["mag"])
	game.hud.toast("NEW HARDWARE — WASP SMG [2]", Color(0.22, 1.0, 0.53))
	game.audio.sfx("quest")

func switch_to(i: int) -> void:
	if i == current or not unlocked[i]:
		return
	reloading = false
	current = i
	game.audio.sfx("ui")

func _physics_process(delta: float) -> void:
	for f in _flashes.duplicate():
		f["t"] = float(f["t"]) - delta
		var fl: OmniLight3D = f["l"]
		fl.light_energy = 3.2 * maxf(0.0, float(f["t"]) / 0.07)
		if float(f["t"]) <= 0.0:
			fl.queue_free()
			_flashes.erase(f)
	for tr in _tracers.duplicate():
		tr["t"] = float(tr["t"]) - delta
		var n: MeshInstance3D = tr["n"]
		var mm: StandardMaterial3D = n.mesh.material
		mm.albedo_color = Color(1, 1, 1, maxf(0.0, float(tr["t"]) / 0.07))
		if float(tr["t"]) <= 0.0:
			n.queue_free()
			_tracers.erase(tr)
	if game == null or game.state != "playing":
		return
	cooldown -= delta
	if reloading:
		reload_t -= delta
		if reload_t <= 0.0:
			reloading = false
			var take := mini(int(_def()["mag"]) - mags[current], reserves[current])
			mags[current] += take
			reserves[current] -= take
	if Input.is_action_just_pressed("reload"):
		reload()
	if Input.is_action_just_pressed("weapon_1"):
		switch_to(0)
	if Input.is_action_just_pressed("weapon_2"):
		switch_to(1)
	var want_fire: bool = Input.is_action_just_pressed("fire") or (Input.is_action_pressed("fire") and bool(_def()["auto"]))
	if want_fire and not reloading and cooldown <= 0.0 and not game.player.in_car:
		if mags[current] > 0:
			fire()
		else:
			reload()

func reload() -> void:
	if reloading or reserves[current] <= 0 or mags[current] >= int(_def()["mag"]):
		return
	reloading = true
	reload_t = float(_def()["reload"])
	game.hud.toast("RELOADING", Color(1.0, 0.84, 0.04))
	game.audio.sfx("ui")

func fire() -> void:
	mags[current] -= 1
	cooldown = float(_def()["rate"])
	var head: Node3D = game.player.head
	var from: Vector3 = head.global_position
	var to: Vector3 = from + (-head.global_transform.basis.z) * 140.0
	var q := PhysicsRayQueryParameters3D.create(from, to)
	q.exclude = [game.player.get_rid()]
	var hit: Dictionary = game.get_world_3d().direct_space_state.intersect_ray(q)
	if not hit.is_empty():
		var col: Object = hit["collider"]
		if col != null and col.is_in_group("enemies"):
			col.damage(float(_def()["dmg"]) * game.player.dmg_mult)
			game.hud.toast("HIT", Color(0.24, 0.95, 1.0))
			game.audio.sfx("hit")
	var hb: Vector3 = game.player.head.global_transform.basis.x
	var hfwd: Vector3 = game.player.head.global_transform.basis.z
	var muzzle: Vector3 = from + hb * 0.22 + Vector3(0, -0.1, 0) - hfwd * 0.35
	var endp: Vector3 = hit["position"] if not hit.is_empty() else to
	_muzzle_flash()
	_tracer(muzzle, endp)
	if not hit.is_empty():
		_sparks(endp)
	game.audio.sfx("shoot")

func _muzzle_flash() -> void:
	var light := OmniLight3D.new()
	light.light_color = Color(1.0, 0.8, 0.45)
	light.light_energy = 3.2
	light.omni_range = 8.0
	light.position = Vector3(0.2, -0.08, -0.35)
	game.player.head.add_child(light)
	_flashes.append({"l": light, "t": 0.07})

func _tracer(a: Vector3, b: Vector3) -> void:
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = Vector3(0.025, 0.025, a.distance_to(b))
	var mat := StandardMaterial3D.new()
	mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	mat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	mat.emission_enabled = true
	mat.emission = Color(0.7, 0.95, 1.0)
	mat.emission_energy_multiplier = 3.0
	bm.material = mat
	mi.mesh = bm
	mi.position = (a + b) / 2.0
	mi.look_at(b, Vector3.UP)
	add_child(mi)
	_tracers.append({"n": mi, "t": 0.07})

func _sparks(pos: Vector3) -> void:
	var p := CPUParticles3D.new()
	p.one_shot = true
	p.emitting = true
	p.amount = 14
	p.lifetime = 0.5
	p.explosiveness = 1.0
	p.spread = 60.0
	p.gravity = Vector3(0, -12, 0)
	p.initial_velocity_min = 2.0
	p.initial_velocity_max = 5.0
	var m := BoxMesh.new()
	m.size = Vector3(0.05, 0.05, 0.05)
	var mat := StandardMaterial3D.new()
	mat.emission_enabled = true
	mat.emission = Color(1.0, 0.7, 0.3)
	mat.emission_energy_multiplier = 3.0
	m.material = mat
	p.mesh = m
	p.position = pos
	add_child(p)
	get_tree().create_timer(0.7).timeout.connect(p.queue_free)
