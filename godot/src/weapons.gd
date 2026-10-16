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
	game.audio.sfx("shoot")
