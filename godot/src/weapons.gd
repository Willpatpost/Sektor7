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
var vm: Node3D
var vm_pistol: Node3D
var vm_smg: Node3D
var _flash: MeshInstance3D
var _kick := 0.0
var _raise := 0.0

const VM_POS := Vector3(0.3, -0.26, -0.55)

func setup(g) -> void:
	game = g
	_build_viewmodel()

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
	_raise = 1.0
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
	if vm != null:
		var pov_first: bool = game.pov == null or game.pov.mode() == "first"
		vm.visible = pov_first and not game.player.in_car
		vm_pistol.visible = current == 0
		vm_smg.visible = current == 1
		_kick = maxf(0.0, _kick - delta * 9.0)
		_raise = maxf(0.0, _raise - delta * 4.0)
		_flash.visible = _kick > 0.55
		var speed01 := clampf(Vector2(game.player.velocity.x, game.player.velocity.z).length() / 8.0, 0.0, 1.0)
		var bob := Vector3(cos(game.player.walk_phase * 0.5) * 0.006, sin(game.player.walk_phase) * 0.009, 0.0) * speed01
		vm.position = VM_POS + bob + Vector3(0.0, -_raise * 0.18, _kick * 0.06)
		vm.rotation = Vector3(_kick * 0.13 - _raise * 0.7, 0.06, 0.0)
		if reloading:
			var rp := clampf(1.0 - reload_t / float(_def()["reload"]), 0.0, 1.0)
			vm.rotation.x -= sin(PI * rp) * 0.85
			vm.position.y -= sin(PI * rp) * 0.1
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
	_kick = 1.0
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
	add_child(mi)
	mi.look_at(b, Vector3.UP)
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

func _build_viewmodel() -> void:
	vm = Node3D.new()
	vm.position = VM_POS
	vm.rotation = Vector3(0.0, 0.06, 0.0)
	vm.visible = false
	game.cam.add_child(vm)
	vm_pistol = _gun_mesh(false)
	vm_smg = _gun_mesh(true)
	vm_smg.visible = false
	vm.add_child(vm_pistol)
	vm.add_child(vm_smg)
	_flash = MeshInstance3D.new()
	var fm := QuadMesh.new()
	fm.size = Vector2(0.16, 0.16)
	var fmat := StandardMaterial3D.new()
	fmat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	fmat.emission_enabled = true
	fmat.emission = Color(1.0, 0.82, 0.45)
	fmat.emission_energy_multiplier = 5.0
	fmat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	fmat.albedo_color = Color(1.0, 0.82, 0.45, 0.9)
	fmat.cull_mode = BaseMaterial3D.CULL_DISABLED
	fm.material = fmat
	_flash.mesh = fm
	_flash.position = Vector3(0, 0.03, -0.44)
	_flash.visible = false
	vm.add_child(_flash)

func _gun_mesh(smg: bool) -> Node3D:
	var root := Node3D.new()
	var metal := StandardMaterial3D.new()
	metal.albedo_color = Color(0.085, 0.09, 0.105)
	metal.metallic = 0.85
	metal.roughness = 0.38
	var dark := StandardMaterial3D.new()
	dark.albedo_color = Color(0.04, 0.045, 0.055)
	dark.metallic = 0.6
	dark.roughness = 0.6
	var glow := StandardMaterial3D.new()
	glow.emission_enabled = true
	glow.emission = Color(0.24, 0.95, 1.0)
	glow.emission_energy_multiplier = 2.4
	if smg:
		_vm_box(root, Vector3(0.07, 0.1, 0.52), Vector3(0, 0, -0.1), metal)
		_vm_box(root, Vector3(0.05, 0.05, 0.3), Vector3(0, 0.06, -0.28), metal)
		_vm_box(root, Vector3(0.045, 0.16, 0.06), Vector3(0, -0.12, -0.16), dark)
		_vm_box(root, Vector3(0.05, 0.05, 0.14), Vector3(0, -0.04, -0.34), dark)
		_vm_box(root, Vector3(0.06, 0.09, 0.16), Vector3(0, -0.02, 0.16), dark)
		_vm_box(root, Vector3(0.012, 0.012, 0.26), Vector3(0, 0.1, -0.2), glow)
	else:
		_vm_box(root, Vector3(0.07, 0.075, 0.34), Vector3(0, 0.02, -0.05), metal)
		_vm_box(root, Vector3(0.06, 0.05, 0.28), Vector3(0, -0.04, -0.03), dark)
		_vm_box(root, Vector3(0.055, 0.16, 0.085), Vector3(0, -0.13, 0.07), dark)
		_vm_box(root, Vector3(0.014, 0.014, 0.014), Vector3(0, 0.07, -0.19), glow)
		_vm_box(root, Vector3(0.03, 0.014, 0.012), Vector3(0, 0.07, 0.09), glow)
	return root

func _vm_box(parent: Node3D, size: Vector3, pos: Vector3, mat: StandardMaterial3D) -> void:
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = size
	mi.mesh = bm
	mi.position = pos
	mi.material_override = mat
	parent.add_child(mi)
