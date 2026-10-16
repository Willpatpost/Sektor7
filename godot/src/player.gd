extends CharacterBody3D

var game
var cam: Camera3D
var head: Node3D
var body: Node3D
var leg_l: MeshInstance3D
var leg_r: MeshInstance3D
var body_head: MeshInstance3D
var hp := 100
var max_hp := 100
var level := 1
var xp := 0
var eddies := 50
var dmg_mult := 1.0
var speed_mult := 1.0
var armor_mult := 1.0
var in_car := false
var walk_phase := 0.0
var _saved_layer := 1
var _saved_mask := 1

const SPEED := 5.2
const SPRINT_MULT := 1.55
const JUMP_VEL := 5.4
const GRAVITY := 15.0

func setup(g, camera: Camera3D) -> void:
	game = g
	cam = camera
	position = game.city.pois.spawn
	head = Node3D.new()
	head.position.y = 1.62
	add_child(head)
	var shape := CollisionShape3D.new()
	var cap := CapsuleShape3D.new()
	cap.radius = 0.45
	cap.height = 1.75
	shape.shape = cap
	shape.position = Vector3(0, 0.875, 0)
	add_child(shape)
	_build_body()
	var rain := CPUParticles3D.new()
	rain.emitting = true
	rain.amount = 600
	rain.lifetime = 1.3
	rain.emission_shape = CPUParticles3D.EMISSION_SHAPE_BOX
	rain.emission_box_extents = Vector3(26, 16, 26)
	rain.direction = Vector3(0.12, -1, 0)
	rain.spread = 5.0
	rain.gravity = Vector3(0, -22, 0)
	rain.initial_velocity_min = 10.0
	rain.initial_velocity_max = 16.0
	var rmesh := BoxMesh.new()
	rmesh.size = Vector3(0.02, 0.55, 0.02)
	rain.mesh = rmesh
	add_child(rain)
	add_to_group("player")

func _box(parent: Node3D, size: Vector3, pos: Vector3, mat: StandardMaterial3D) -> MeshInstance3D:
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = size
	mi.mesh = bm
	mi.position = pos
	mi.material_override = mat
	parent.add_child(mi)
	return mi

func _build_body() -> void:
	body = Node3D.new()
	var cloth := StandardMaterial3D.new()
	cloth.albedo_color = Color(0.12, 0.17, 0.21)
	var dark := StandardMaterial3D.new()
	dark.albedo_color = Color(0.07, 0.09, 0.11)
	var glow := StandardMaterial3D.new()
	glow.emission_enabled = true
	glow.emission = Color(0.24, 0.95, 1.0)
	glow.emission_energy_multiplier = 2.0
	leg_l = _box(body, Vector3(0.16, 0.85, 0.2), Vector3(-0.12, 0.42, 0), dark)
	leg_r = _box(body, Vector3(0.16, 0.85, 0.2), Vector3(0.12, 0.42, 0), dark)
	_box(body, Vector3(0.5, 0.62, 0.28), Vector3(0, 1.12, 0), cloth)
	_box(body, Vector3(0.52, 0.06, 0.3), Vector3(0, 1.32, 0), glow)
	body_head = _box(body, Vector3(0.24, 0.26, 0.26), Vector3(0, 1.58, 0), dark)
	_box(body_head, Vector3(0.26, 0.06, 0.06), Vector3(0, 0.02, 0.14), glow)
	_box(body, Vector3(0.08, 0.1, 0.5), Vector3(0.3, 1.08, 0.25), dark)
	body.visible = false
	add_child(body)

func on_look(rel_x: float, rel_y: float) -> void:
	rotate_y(-rel_x * 0.0021)
	head.rotation.x = clampf(head.rotation.x - rel_y * 0.0021, -1.45, 1.45)

func _physics_process(delta: float) -> void:
	if game == null or game.state != "playing" or in_car:
		return
	var input2d := Input.get_vector("move_left", "move_right", "move_back", "move_forward")
	var mult := SPRINT_MULT if Input.is_action_pressed("sprint") else 1.0
	var dir := transform.basis * Vector3(input2d.x, 0, -input2d.y)
	velocity.x = dir.x * SPEED * speed_mult * mult
	velocity.z = dir.z * SPEED * speed_mult * mult
	if is_on_floor():
		if Input.is_action_just_pressed("jump"):
			velocity.y = JUMP_VEL
	else:
		velocity.y -= GRAVITY * delta
	move_and_slide()
	if body.visible:
		var moving := Vector2(velocity.x, velocity.z).length() > 0.5
		walk_phase += delta * (8.0 if moving else 0.0)
		var amp := 0.55 if moving else 0.0
		leg_l.rotation.x = sin(walk_phase) * amp
		leg_r.rotation.x = -sin(walk_phase) * amp
		body_head.rotation.x = head.rotation.x * 0.6

func damage(n: int) -> bool:
	hp = maxi(0, hp - int(n * armor_mult))
	if hp <= 0:
		game.on_player_death()
		return true
	return false

func heal(n: int) -> void:
	hp = mini(max_hp, hp + n)

func add_xp(n: int) -> void:
	xp += n
	var next := 90 + level * 70
	while xp >= next:
		xp -= next
		level += 1
		max_hp += 20
		hp = max_hp
		dmg_mult += 0.05
		game.hud.toast("LEVEL UP — LVL %d" % level, Color(0.22, 1.0, 0.53))
		game.audio.sfx("level")
		next = 90 + level * 70

func reset() -> void:
	position = game.city.pois.spawn
	hp = max_hp
	velocity = Vector3.ZERO

func set_collisions_enabled(on: bool) -> void:
	if on:
		collision_layer = _saved_layer
		collision_mask = _saved_mask
	else:
		_saved_layer = collision_layer
		_saved_mask = collision_mask
		collision_layer = 0
		collision_mask = 0
