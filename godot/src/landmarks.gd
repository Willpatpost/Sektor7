extends Node

## Hero landmark structures on the five reserved POI cells.
## Authored (no rng) so each is identical every run; masses are offset to keep
## POI ground clear for spawns and combat, and every solid registers into
## city.colliders so prop scatter, hovercar landings, and hitscan respect them.
## Faces the shared material library; signs/holo/beacons reuse city helpers.

var game
var _built := 0

func setup(g) -> void:
	game = g

func count() -> int:
	return _built

func build() -> void:
	_spire()
	_bunker()
	_canopy()
	_arena()
	_mast()

func _spire() -> void:
	var root := Node3D.new()
	add_child(root)
	_collide(75.0, 39.0, 18.0, 18.0, 8.0)
	_collide(75.0, 39.0, 13.0, 13.0, 22.0)
	_collide(75.0, 39.0, 9.0, 9.0, 34.0)
	var conc: Material = game.materials.std_mat("facade", Color(0.52, 0.56, 0.64), 0.7, 0.1)
	_box(root, Vector3(18, 8, 18), Vector3(75, 4, 39), conc)
	_box(root, Vector3(13, 14, 13), Vector3(75, 15, 39), conc)
	_box(root, Vector3(9, 12, 9), Vector3(75, 28, 39), conc)
	var lip: Material = game.materials.std_mat("metal", Color(0.42, 0.45, 0.52), 0.5, 0.6)
	_box(root, Vector3(18.6, 0.5, 18.6), Vector3(75, 8.1, 39), lip)
	_box(root, Vector3(13.6, 0.5, 13.6), Vector3(75, 22.1, 39), lip)
	_box(root, Vector3(9.6, 0.5, 9.6), Vector3(75, 34.1, 39), lip)
	var cyan := Color(0.24, 0.95, 1.0)
	for sx in [-4.2, 4.2]:
		for sz in [-4.2, 4.2]:
			_glow(root, Vector3(75 + sx, 28.0, 39 + sz), Vector3(0.12, 10.0, 0.12), cyan, 2.4)
	_glow(root, Vector3(75, 34.8, 34.2), Vector3(10.0, 0.14, 0.14), cyan, 3.0)
	_glow(root, Vector3(75, 34.8, 43.8), Vector3(10.0, 0.14, 0.14), cyan, 3.0)
	_glow(root, Vector3(70.2, 34.8, 39), Vector3(0.14, 0.14, 10.0), cyan, 3.0)
	_glow(root, Vector3(79.8, 34.8, 39), Vector3(0.14, 0.14, 10.0), cyan, 3.0)
	var ant := MeshInstance3D.new()
	var am := CylinderMesh.new()
	am.top_radius = 0.07
	am.bottom_radius = 0.09
	am.height = 6.0
	ant.mesh = am
	ant.position = Vector3(75, 37, 39)
	ant.material_override = game.materials.std_mat("metal", Color(0.4, 0.42, 0.47), 0.5, 0.8)
	root.add_child(ant)
	_beacon(root, Vector3(75, 40.4, 39))
	game.city._holo(75.0, 24.0, 32.3, 8.0, 4.0, PI)
	game.city._sign(75.0, 5.6, 29.6, PI, "AZURE SPIRE", cyan)
	_built += 1

func _bunker() -> void:
	var root := Node3D.new()
	add_child(root)
	_collide(-125.0, -38.0, 20.0, 12.0, 7.0)
	var rust: Material = game.materials.std_mat("metal", Color(0.42, 0.38, 0.36), 0.75, 0.3)
	_box(root, Vector3(20, 7, 12), Vector3(-125, 3.5, -38), rust)
	var lip: Material = game.materials.std_mat("metal", Color(0.3, 0.3, 0.33), 0.6, 0.4)
	_box(root, Vector3(20.5, 0.4, 12.5), Vector3(-125, 7.1, -38), lip)
	var cyan := Color(0.24, 0.95, 1.0)
	for sx in [-6.0, 0.0, 6.0]:
		_glow(root, Vector3(-125 + sx, 4.6, -31.92), Vector3(2.2, 0.16, 0.1), cyan, 2.0)
	_glow(root, Vector3(-134.2, 7.5, -31.9), Vector3(0.35, 0.25, 0.25), Color(1.0, 0.9, 0.7), 3.0)
	_glow(root, Vector3(-115.8, 7.5, -31.9), Vector3(0.35, 0.25, 0.25), Color(1.0, 0.9, 0.7), 3.0)
	var post: Material = game.materials.std_mat("metal", Color(0.35, 0.36, 0.4), 0.6, 0.6)
	for i in 6:
		_box(root, Vector3(0.18, 2.4, 0.18), Vector3(-134.0 + i * 3.6, 1.2, -30.6), post)
	game.city._sign(-125.0, 8.4, -31.8, 0.0, "DETENTION", Color(1.0, 0.16, 0.43))
	_built += 1

func _canopy() -> void:
	var root := Node3D.new()
	add_child(root)
	var post: Material = game.materials.std_mat("metal", Color(0.4, 0.42, 0.47), 0.55, 0.6)
	for px in [34.5, 41.5]:
		for pz in [-79.0, -71.0]:
			_collide(px, pz, 0.5, 0.5, 3.4)
			_box(root, Vector3(0.35, 3.4, 0.35), Vector3(px, 1.7, pz), post)
	var roof: Material = game.materials.std_mat("metal", Color(0.5, 0.42, 0.3), 0.65, 0.3)
	_box(root, Vector3(9.0, 0.3, 10.0), Vector3(38, 3.6, -75), roof)
	for pz in [-77.4, -72.6]:
		_glow(root, Vector3(38, 3.3, pz), Vector3(7.0, 0.08, 0.3), Color(1.0, 0.84, 0.04), 2.6)
	var stall: Material = game.materials.std_mat("metal", Color(0.45, 0.47, 0.52), 0.6, 0.3)
	_box(root, Vector3(2.2, 0.9, 1.1), Vector3(36, 0.45, -76.8), stall)
	_box(root, Vector3(2.2, 0.9, 1.1), Vector3(40, 0.45, -73.4), stall)
	game.city._sign(32.4, 4.4, -75.0, -PI / 2.0, "NIGHT MARKET", Color(1.0, 0.84, 0.04))
	_built += 1

func _arena() -> void:
	var root := Node3D.new()
	add_child(root)
	var dark: Material = game.materials.std_mat("metal", Color(0.3, 0.3, 0.34), 0.6, 0.5)
	var red := Color(1.0, 0.13, 0.2)
	for tx in [111.0, 139.0]:
		_collide(tx, -125.0, 8.0, 8.0, 20.0)
		_box(root, Vector3(8, 20, 8), Vector3(tx, 10, -125), dark)
		var inner := 4.2 if tx < 125.0 else -4.2
		for sy in [4.0, 12.0]:
			_glow(root, Vector3(tx + inner, sy, -128.9), Vector3(0.1, 6.0, 0.1), red, 2.2)
			_glow(root, Vector3(tx + inner, sy, -121.1), Vector3(0.1, 6.0, 0.1), red, 2.2)
	_box(root, Vector3(20.0, 1.5, 2.0), Vector3(125, 18, -125), dark)
	_glow(root, Vector3(125, 18.9, -125), Vector3(19.0, 0.12, 0.2), red, 2.6)
	game.city._sign(115.6, 6.0, -125.0, PI / 2.0, "RAZORTAIL", red, true)
	_built += 1

func _mast() -> void:
	var root := Node3D.new()
	add_child(root)
	_collide(-25.0, 125.0, 3.4, 3.4, 30.0)
	var steel: Material = game.materials.std_mat("metal", Color(0.5, 0.52, 0.58), 0.5, 0.8)
	var red := Color(1.0, 0.13, 0.16)
	var widths := [3.2, 2.8, 2.4, 2.0, 1.6]
	for i in 5:
		var w: float = widths[i]
		var mi := _box(root, Vector3(w, 6.0, w), Vector3(-25, 3.0 + i * 6.0, 125), steel)
		mi.rotation.y = PI / 4.0 if i % 2 == 1 else 0.0
		_glow(root, Vector3(-25, 5.7 + i * 6.0, 125), Vector3(w + 0.4, 0.22, w + 0.4), red, 1.8)
	_beacon(root, Vector3(-25, 10.4, 125))
	_beacon(root, Vector3(-25, 20.4, 125))
	_beacon(root, Vector3(-25, 30.5, 125))
	for dx in [-1.4, 1.4]:
		var dish := _box(root, Vector3(1.2, 0.8, 0.3), Vector3(-25 + dx, 14.0, 125), steel)
		dish.rotation.z = dx * 0.4
	game.city._sign(-23.2, 5.4, 125.0, PI / 2.0, "SEKTOR BROADCAST", Color(0.24, 0.95, 1.0))
	_built += 1

func _box(parent: Node3D, size: Vector3, pos: Vector3, mat: Material) -> MeshInstance3D:
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = size
	bm.material = mat
	mi.mesh = bm
	mi.position = pos
	parent.add_child(mi)
	return mi

func _glow(parent: Node3D, pos: Vector3, size: Vector3, col: Color, energy := 2.2) -> MeshInstance3D:
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = size
	mi.mesh = bm
	mi.position = pos
	var bmat := StandardMaterial3D.new()
	bmat.emission_enabled = true
	bmat.emission = col
	bmat.emission_energy_multiplier = energy
	bmat.albedo_color = Color(0.04, 0.04, 0.06)
	mi.material_override = bmat
	parent.add_child(mi)
	return mi

func _beacon(parent: Node3D, pos: Vector3) -> void:
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = Vector3(0.18, 0.18, 0.18)
	mi.mesh = bm
	mi.position = pos
	var bmat := StandardMaterial3D.new()
	bmat.emission_enabled = true
	bmat.emission = Color(1.0, 0.13, 0.16)
	bmat.emission_energy_multiplier = 2.0
	mi.material_override = bmat
	parent.add_child(mi)
	game.city._blinkers.append({"mat": bmat, "ph": float(_built) * 2.7})

func _collide(cx: float, cz: float, w: float, d: float, h: float) -> void:
	var body := StaticBody3D.new()
	body.position = Vector3(cx, h / 2.0, cz)
	var shape := CollisionShape3D.new()
	var box := BoxShape3D.new()
	box.size = Vector3(w, h, d)
	shape.shape = box
	body.add_child(shape)
	add_child(body)
	game.city.colliders.append({"min_x": cx - w / 2.0, "max_x": cx + w / 2.0, "min_z": cz - d / 2.0, "max_z": cz + d / 2.0, "h": h})
