extends Node3D

const CELL := 50
const GRID := 8
const HALF := 200.0

var pois := {}
var colliders := []
var rng := RandomNumberGenerator.new()
var _mat_cache := {}

const SIGN_WORDS := ["RAMEN", "CYBERWARE", "MOTEL", "BAR", "X-CHANGE", "TECH 24H", "CLUB", "KARAOKE", "HOTEL", "SUSHI", "VOLT", "NEON"]
const NEON := [Color(0.24, 0.95, 1.0), Color(1.0, 0.16, 0.43), Color(1.0, 0.84, 0.04), Color(0.22, 1.0, 0.53), Color(0.69, 0.15, 1.0)]

func build(_game: Node3D) -> void:
	rng.seed = 1337
	_build_environment()
	_build_ground()
	var special := {"5,4": true, "1,3": true, "4,2": true, "6,1": true, "3,6": true}
	for col in GRID:
		for row in GRID:
			var key := "%d,%d" % [col, row]
			if special.has(key):
				continue
			var cx := -HALF + col * CELL + CELL / 2.0
			var cz := -HALF + row * CELL + CELL / 2.0
			if rng.randf() < 0.35:
				_building(cx + rng.randf_range(-2, 2), cz + rng.randf_range(-2, 2),
					rng.randf_range(16, 26), rng.randf_range(16, 26), rng.randf_range(9, 16))
			else:
				for s in [-1.0, 1.0]:
					_building(cx + s * 6.5, cz, rng.randf_range(9, 12), rng.randf_range(19, 26), rng.randf_range(18, 42))
	_build_perimeter()
	_build_lamps()
	_build_arch()
	pois = {
		"spawn": Vector3(0, 0, 146),
		"plaza": Vector3(75, 0, 25),
		"lockup": Vector3(-125, 0, -16.5),
		"market": Vector3(25, 0, -75),
		"boss": Vector3(125, 0, -125)
	}

func _building(x: float, z: float, w: float, d: float, h: float) -> void:
	var body := StaticBody3D.new()
	body.position = Vector3(x, h / 2.0, z)
	var shape := CollisionShape3D.new()
	var box := BoxShape3D.new()
	box.size = Vector3(w, h, d)
	shape.shape = box
	body.add_child(shape)
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = Vector3(w, h, d)
	mi.mesh = bm
	var bucket := 0 if h < 17.0 else (1 if h < 42.0 else 2)
	mi.material_override = _wall_material(bucket)
	body.add_child(mi)
	add_child(body)
	colliders.append({"min_x": x - w / 2.0, "max_x": x + w / 2.0, "min_z": z - d / 2.0, "max_z": z + d / 2.0, "h": h})
	if h > 50.0:
		_glow_box(body, Vector3(x - body.position.x, h - body.position.y + 3.0, z - body.position.z), Vector3(0.4, 0.4, 0.4), Color(1.0, 0.13, 0.2))
	if rng.randf() < 0.6:
		_sign(x, rng.randf_range(4.0, minf(h - 2.0, 18.0)), z + d / 2.0 + 0.2, 0.0,
			SIGN_WORDS[rng.randi() % SIGN_WORDS.size()], NEON[rng.randi() % NEON.size()])

func _wall_material(bucket: int) -> StandardMaterial3D:
	if _mat_cache.has(bucket):
		return _mat_cache[bucket]
	var rows: int = [8, 14, 22][bucket]
	var cols := 12
	var img := Image.create_empty(256, 512, false, Image.FORMAT_RGB8)
	img.fill(Color(0.07, 0.08, 0.11))
	var r2 := RandomNumberGenerator.new()
	r2.seed = bucket * 991 + 5
	var cw := 256 / cols
	var chh := 512 / rows
	for r in rows:
		for q in cols:
			var lit := r2.randf() < 0.34
			var col := Color(0.02, 0.027, 0.047)
			if lit:
				var base: Color = [Color(1, 0.85, 0.63), Color(0.62, 0.85, 1), Color(1, 0.6, 0.84)][r2.randi() % 3]
				col = base * Color(1, 1, 1) * r2.randf_range(0.35, 1.0)
			img.fill_rect(Rect2i(q * cw + 3, r * chh + 3, cw - 6, chh - 7), col)
	var tex := ImageTexture.create_from_image(img)
	var mat := StandardMaterial3D.new()
	mat.albedo_texture = tex
	mat.emission_enabled = true
	mat.emission_texture = tex
	mat.emission_energy_multiplier = 1.1
	mat.roughness = 0.9
	_mat_cache[bucket] = mat
	return mat

func _glow_box(parent: Node3D, pos: Vector3, size: Vector3, col: Color) -> void:
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = size
	mi.mesh = bm
	mi.position = pos
	var mat := StandardMaterial3D.new()
	mat.emission_enabled = true
	mat.emission = col
	mat.emission_energy_multiplier = 2.0
	mat.albedo_color = Color(0.05, 0.05, 0.07)
	mi.material_override = mat
	parent.add_child(mi)

func _sign(x: float, y: float, z: float, ry: float, text: String, col: Color) -> void:
	var l := Label3D.new()
	l.text = text
	l.position = Vector3(x, y, z)
	l.rotation.y = ry
	l.modulate = col
	l.font_size = 64
	l.outline_size = 8
	add_child(l)

func _build_environment() -> void:
	var we := WorldEnvironment.new()
	var env := Environment.new()
	var sky := Sky.new()
	var sky_mat := ProceduralSkyMaterial.new()
	sky_mat.sky_top_color = Color(0.01, 0.02, 0.05)
	sky_mat.sky_horizon_color = Color(0.32, 0.12, 0.29)
	sky_mat.ground_bottom_color = Color(0.02, 0.02, 0.04)
	sky_mat.ground_horizon_color = Color(0.25, 0.1, 0.24)
	sky_mat.sun_angle_max = 20.0
	sky.sky_material = sky_mat
	env.set("background", Environment.BG_SKY)
	env.sky = sky
	env.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
	env.ambient_light_energy = 1.2
	env.fog_enabled = true
	env.fog_light_color = Color(0.04, 0.05, 0.09)
	env.fog_density = 0.004
	env.volumetric_fog_enabled = true
	env.volumetric_fog_density = 0.012
	env.volumetric_fog_albedo = Color(0.5, 0.55, 0.75)
	env.glow_enabled = true
	env.glow_intensity = 1.2
	env.glow_bloom = 0.1
	env.glow_hdr_threshold = 0.9
	we.environment = env
	add_child(we)
	var moon := DirectionalLight3D.new()
	moon.rotation = Vector3(deg_to_rad(-55), deg_to_rad(30), 0)
	moon.light_color = Color(0.55, 0.6, 1.0)
	moon.light_energy = 0.5
	moon.shadow_enabled = true
	add_child(moon)

func _build_ground() -> void:
	var body := StaticBody3D.new()
	body.position = Vector3(0, -0.25, 0)
	var shape := CollisionShape3D.new()
	var box := BoxShape3D.new()
	box.size = Vector3(470, 0.5, 470)
	shape.shape = box
	body.add_child(shape)
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = Vector3(470, 0.5, 470)
	mi.mesh = bm
	var mat := StandardMaterial3D.new()
	mat.albedo_color = Color(0.045, 0.055, 0.09)
	mat.roughness = 0.25
	mat.metallic = 0.6
	mi.material_override = mat
	body.add_child(mi)
	add_child(body)

func _build_perimeter() -> void:
	var t := -180.0
	while t <= 180.0:
		for spot in [[t, 215.0], [t, -215.0], [215.0, t], [-215.0, t]]:
			_building(spot[0], spot[1], rng.randf_range(26, 34), rng.randf_range(26, 34), rng.randf_range(55, 110))
		t += 36.0

func _build_lamps() -> void:
	var lines := [-150, -100, -50, 0, 50, 100, 150]
	var i := 0
	for lx in lines:
		for lz in lines:
			var lamp := Node3D.new()
			lamp.position = Vector3(lx + 8.5, 0, lz + 8.5)
			_glow_box(lamp, Vector3(0, 5.6, 0), Vector3(0.12, 0.12, 1.5), Color(0.6, 0.85, 1.0))
			var pole := MeshInstance3D.new()
			var pm := BoxMesh.new()
			pm.size = Vector3(0.14, 5.6, 0.14)
			pole.mesh = pm
			pole.position = Vector3(0, 2.8, 0)
			var pmat := StandardMaterial3D.new()
			pmat.albedo_color = Color(0.1, 0.12, 0.16)
			pole.material_override = pmat
			lamp.add_child(pole)
			if i % 4 == 0:
				var omni := OmniLight3D.new()
				omni.position = Vector3(0, 5.2, 0)
				omni.light_color = Color(0.5, 0.75, 1.0)
				omni.omni_range = 14.0
				omni.light_energy = 1.2
				lamp.add_child(omni)
			add_child(lamp)
			i += 1

func _build_arch() -> void:
	for s in [-7.0, 7.0]:
		var body := StaticBody3D.new()
		body.position = Vector3(s, 4.5, 150)
		var shape := CollisionShape3D.new()
		var box := BoxShape3D.new()
		box.size = Vector3(1.2, 9, 1.2)
		shape.shape = box
		body.add_child(shape)
		add_child(body)
	_sign(0, 9.6, 150.4, 0.0, "SEKTOR 7", Color(0.24, 0.95, 1.0))
