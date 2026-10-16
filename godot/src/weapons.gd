extends Node

var game
var mag := 12
var reserve := 48
var cooldown := 0.0
var reloading := false
var reload_t := 0.0

const DMG := 34.0
const RATE := 0.16
const RELOAD := 1.05
const MAG_SIZE := 12

func setup(g: Node3D) -> void:
	game = g

func _physics_process(delta: float) -> void:
	if game == null or game.state != "playing":
		return
	cooldown -= delta
	if reloading:
		reload_t -= delta
		if reload_t <= 0.0:
			reloading = false
			var take := mini(MAG_SIZE - mag, reserve)
			mag += take
			reserve -= take
	if Input.is_action_just_pressed("reload"):
		reload()
	if Input.is_action_just_pressed("fire") and not reloading and cooldown <= 0.0 and not game.player.in_car:
		if mag > 0:
			fire()
		else:
			reload()

func reload() -> void:
	if reloading or reserve <= 0 or mag >= MAG_SIZE:
		return
	reloading = true
	reload_t = RELOAD
	game.hud.toast("RELOADING", Color(1.0, 0.84, 0.04))

func fire() -> void:
	mag -= 1
	cooldown = RATE
	var cam: Camera3D = game.player.cam
	var from: Vector3 = cam.global_position
	var to: Vector3 = from + (-cam.global_transform.basis.z) * 140.0
	var q := PhysicsRayQueryParameters3D.create(from, to)
	q.exclude = [game.player.get_rid()]
	var hit: Dictionary = game.get_world_3d().direct_space_state.intersect_ray(q)
	if not hit.is_empty():
		var col: Object = hit["collider"]
		if col != null and col.is_in_group("enemies"):
			col.damage(DMG * game.player.dmg_mult)
			game.hud.toast("HIT", Color(0.24, 0.95, 1.0))
