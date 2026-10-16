extends Node

var game
var cam: Camera3D
var modes := [
	{"id": "first", "label": "FIRST PERSON"},
	{"id": "behind_close", "label": "THIRD PERSON — CLOSE"},
	{"id": "behind_far", "label": "THIRD PERSON — FAR"},
	{"id": "front_close", "label": "FRONT — CLOSE"},
	{"id": "front_far", "label": "FRONT — FAR"}
]
var idx := 0

func setup(g, camera: Camera3D) -> void:
	game = g
	cam = camera

func mode() -> String:
	return modes[idx]["id"]

func cycle() -> void:
	idx = (idx + 1) % modes.size()
	game.hud.toast("POV — " + modes[idx]["label"], Color(0.62, 0.85, 1.0))
	game.audio.sfx("ui")

func _physics_process(delta: float) -> void:
	if game == null or game.player == null:
		return
	var head: Node3D = game.player.head
	var eye: Vector3 = head.global_position
	var bq: Quaternion = head.global_transform.basis.get_rotation_quaternion()
	var fwd: Vector3 = -head.global_transform.basis.z
	var dist := 0.0
	var lift := 0.0
	var is_front := false
	match mode():
		"behind_close":
			dist = 2.8
			lift = 0.35
		"behind_far":
			dist = 6.0
			lift = 1.1
		"front_close":
			dist = 2.6
			lift = 0.15
			is_front = true
		"front_far":
			dist = 5.5
			lift = 0.7
			is_front = true
	var target := eye
	var target_q := bq
	if dist > 0.0:
		var ray := fwd if is_front else -fwd
		var d := dist
		var q := PhysicsRayQueryParameters3D.create(eye, eye + ray * (dist + 0.4))
		q.exclude = [game.player.get_rid()]
		var hit: Dictionary = game.get_world_3d().direct_space_state.intersect_ray(q)
		if not hit.is_empty():
			var hd: float = hit["position"].distance_to(eye)
			if hd < dist + 0.4:
				d = maxf(0.4, hd - 0.35)
		target = eye + ray * d + Vector3.UP * lift * (d / dist)
		if is_front:
			target_q = Basis().looking_at((eye - target).normalized(), Vector3.UP).get_rotation_quaternion()
	var k := minf(1.0, delta * 18.0)
	cam.position = cam.position.lerp(target, k)
	cam.quaternion = cam.quaternion.slerp(target_q, k)
	var show_body: bool = mode() != "first" and not game.player.in_car
	game.player.body.visible = show_body
