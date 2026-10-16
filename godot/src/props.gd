extends Node

## 3D prop registry + deterministic city scatter (density pass).
##
## GLTF props live in assets/models/<name>/<name>.gltf (CC0, see CREDITS.md).
## Everything renders through MultiMesh batches - one draw call per prop mesh
## regardless of instance count - plus procedural density elements (overhead
## cables, facade pipes, window AC units, awnings) built from shared meshes.
## Nothing adds collision, so streets and POIs stay walkable; scatter is driven
## by the city's seeded rng and skips occupied spots marked by city builders.

var game
var _parts := {}
var _queue := {}
var _count := 0

const REGISTRY := {
	"trash_can": "res://assets/models/metal_trash_can/metal_trash_can.gltf",
	"trashbag": "res://assets/models/trashbag/trashbag.gltf",
	"cardboard": "res://assets/models/cardboard_box_01/cardboard_box_01.gltf",
	"crate": "res://assets/models/wooden_crate_01/wooden_crate_01.gltf",
	"plastic_crate": "res://assets/models/plastic_crate_01/plastic_crate_01.gltf",
	"bench": "res://assets/models/painted_wooden_bench/painted_wooden_bench.gltf",
	"hydrant": "res://assets/models/fire_hydrant/fire_hydrant.gltf",
	"barrel": "res://assets/models/Barrel_01/Barrel_01.gltf",
	"propane": "res://assets/models/propane_tank/propane_tank.gltf",
	"aircon": "res://assets/models/exterior_aircon_unit/exterior_aircon_unit.gltf",
	"wetsign": "res://assets/models/WetFloorSign_01/WetFloorSign_01.gltf",
}

const SIDEWALK_POOL := [
	"trash_can", "trash_can", "trash_can", "trashbag", "trashbag",
	"cardboard", "cardboard", "crate", "plastic_crate", "barrel",
	"bench", "hydrant", "wetsign", "propane"
]
const ROOF_POOL := ["aircon", "aircon", "aircon", "propane", "barrel"]
const LINES := [-150.0, -100.0, -50.0, 0.0, 50.0, 100.0, 150.0]

func setup(g) -> void:
	game = g
	for key in REGISTRY:
		var path: String = REGISTRY[key]
		if not ResourceLoader.exists(path):
			continue
		var proto: Node3D = load(path).instantiate()
		var parts := []
		_collect(proto, Transform3D.IDENTITY, parts)
		if not parts.is_empty():
			_parts[key] = parts
		proto.free()

func _collect(node: Node3D, acc: Transform3D, parts: Array) -> void:
	acc = acc * node.transform
	if node is MeshInstance3D and node.mesh != null:
		parts.append({"mesh": node.mesh, "local": acc})
	for c in node.get_children():
		if c is Node3D:
			_collect(c, acc, parts)

func placed_count() -> int:
	return _count

func queue(key: String, xform: Transform3D) -> void:
	if not _parts.has(key):
		return
	if not _queue.has(key):
		_queue[key] = []
	_queue[key].append(xform)
	_count += 1

func flush() -> void:
	for key in _queue:
		var xforms: Array = _queue[key]
		for part in _parts[key]:
			_batch(part.mesh, xforms)
	_queue = {}

func _batch(mesh: Mesh, xforms: Array) -> void:
	var mm := MultiMesh.new()
	mm.transform_format = MultiMesh.TRANSFORM_3D
	mm.mesh = mesh
	mm.instance_count = xforms.size()
	for i in xforms.size():
		mm.set_instance_transform(i, xforms[i])
	var mi := MultiMeshInstance3D.new()
	mi.multimesh = mm
	game.city.add_child(mi)

func scatter() -> void:
	var rng: RandomNumberGenerator = game.city.rng
	_scatter_sidewalks(rng)
	_scatter_roofs(rng)
	_scatter_facades(rng)
	_scatter_cables(rng)
	flush()

func _pick(rng: RandomNumberGenerator, pool: Array) -> String:
	var key: String = pool[rng.randi() % pool.size()]
	return key

func _spot_free(pos: Vector3, margin: float) -> bool:
	if absf(pos.x) > 194.0 or absf(pos.z) > 194.0:
		return false
	if pos.distance_to(game.city.pois.spawn) < 6.0:
		return false
	for c in game.city.colliders:
		if pos.x > c.min_x - margin and pos.x < c.max_x + margin and pos.z > c.min_z - margin and pos.z < c.max_z + margin:
			return false
	for o in game.city.occupied:
		if Vector2(pos.x - o.x, pos.z - o.z).length() < 1.7:
			return false
	for l in game.city.lamp_spots:
		if Vector2(pos.x - l.x, pos.z - l.z).length() < 1.3:
			return false
	return true

func _scatter_sidewalks(rng: RandomNumberGenerator) -> void:
	for axis in [0, 1]:
		for lane_v in LINES:
			var lane: float = lane_v
			var t := -188.0
			while t < 188.0:
				t += rng.randf_range(7.0, 15.0)
				var side := 1.0 if rng.randf() < 0.5 else -1.0
				var off := rng.randf_range(9.0, 9.9)
				var px: float = lane + side * off if axis == 0 else t
				var pz: float = t if axis == 0 else lane + side * off
				var pos := Vector3(px, 0, pz)
				if not _spot_free(pos, 1.2):
					continue
				var key := _pick(rng, SIDEWALK_POOL)
				var yaw := rng.randf() * TAU
				if key == "bench":
					yaw = (0.0 if axis == 0 else PI / 2.0) + (0.0 if side > 0.0 else PI) + rng.randf_range(-0.15, 0.15)
				queue(key, Transform3D(Basis(Vector3.UP, yaw), pos))
				if key != "bench" and key != "hydrant" and rng.randf() < 0.35:
					var k2 := _pick(rng, SIDEWALK_POOL)
					var npos := Vector3(px + rng.randf_range(-1.5, 1.5), 0, pz + rng.randf_range(-1.5, 1.5))
					if _spot_free(npos, 1.0):
						queue(k2, Transform3D(Basis(Vector3.UP, rng.randf() * TAU), npos))

func _scatter_roofs(rng: RandomNumberGenerator) -> void:
	for c in game.city.colliders:
		if c.h < 18.0:
			continue
		var cx: float = (c.min_x + c.max_x) / 2.0
		var cz: float = (c.min_z + c.max_z) / 2.0
		var hw: float = (c.max_x - c.min_x) / 4.0
		var hd: float = (c.max_z - c.min_z) / 4.0
		var n := 1 + (1 if rng.randf() < 0.45 else 0)
		for i in n:
			var key := _pick(rng, ROOF_POOL)
			var pos := Vector3(cx + rng.randf_range(-hw, hw), c.h, cz + rng.randf_range(-hd, hd))
			queue(key, Transform3D(Basis(Vector3.UP, rng.randf() * TAU), pos))

func _face_toward(c: Dictionary) -> Dictionary:
	var cx: float = (c.min_x + c.max_x) / 2.0
	var cz: float = (c.min_z + c.max_z) / 2.0
	var nx: float = 50.0 * round(cx / 50.0)
	var nz: float = 50.0 * round(cz / 50.0)
	if absf(cx - nx) <= absf(cz - nz):
		if nx < cx:
			return {"x": c.min_x, "z": cz, "normal": Vector3(-1, 0, 0), "axis": 0}
		return {"x": c.max_x, "z": cz, "normal": Vector3(1, 0, 0), "axis": 0}
	if nz < cz:
		return {"x": cx, "z": c.min_z, "normal": Vector3(0, 0, -1), "axis": 1}
	return {"x": cx, "z": c.max_z, "normal": Vector3(0, 0, 1), "axis": 1}

func _face_yaw(normal: Vector3) -> float:
	if normal.x > 0.5:
		return 0.0
	if normal.x < -0.5:
		return PI
	if normal.z > 0.5:
		return -PI / 2.0
	return PI / 2.0

func _scatter_facades(rng: RandomNumberGenerator) -> void:
	var pipe_mesh := CylinderMesh.new()
	pipe_mesh.top_radius = 0.05
	pipe_mesh.bottom_radius = 0.05
	pipe_mesh.height = 1.0
	pipe_mesh.radial_segments = 6
	pipe_mesh.material = game.materials.std_mat("metal", Color(0.4, 0.42, 0.47), 0.55, 0.8)
	var unit_mesh := BoxMesh.new()
	unit_mesh.size = Vector3(0.42, 0.55, 0.9)
	unit_mesh.material = game.materials.std_mat("metal", Color(0.5, 0.52, 0.57), 0.6, 0.6)
	var awning_mesh := BoxMesh.new()
	awning_mesh.size = Vector3(0.9, 0.05, 2.4)
	awning_mesh.material = game.materials.std_mat("metal", Color(0.35, 0.37, 0.43), 0.7, 0.3)
	var pipes := []
	var units := []
	var awnings := []
	for c in game.city.colliders:
		if c.max_x > 200.0 or c.min_x < -200.0 or c.max_z > 200.0 or c.min_z < -200.0:
			continue
		var h: float = c.h
		if h >= 9.0:
			for corner in [[c.min_x - 0.14, c.min_z - 0.14], [c.max_x + 0.14, c.max_z + 0.14]]:
				var px: float = corner[0]
				var pz: float = corner[1]
				pipes.append(Transform3D(Basis().scaled(Vector3(1.0, h, 1.0)), Vector3(px, h / 2.0, pz)))
		var face := _face_toward(c)
		var yaw := _face_yaw(face["normal"])
		var normal: Vector3 = face["normal"]
		var half: float = (c.max_z - c.min_z) if face["axis"] == 0 else (c.max_x - c.min_x)
		if rng.randf() < 0.75:
			var n_units := rng.randi() % 3
			for i in n_units:
				var along: float = face["z"] + rng.randf_range(-half / 2.0 + 1.2, half / 2.0 - 1.2) if face["axis"] == 0 else face["x"] + rng.randf_range(-half / 2.0 + 1.2, half / 2.0 - 1.2)
				var uy := 2.1 + float(rng.randi() % 3) * 3.0
				var base := Vector3(face["x"], uy, along) if face["axis"] == 0 else Vector3(along, uy, face["z"])
				units.append(Transform3D(Basis(Vector3.UP, yaw), base + normal * 0.24))
		if rng.randf() < 0.6 and h > 6.0:
			var a_along: float = face["z"] + rng.randf_range(-half / 2.0 + 2.0, half / 2.0 - 2.0) if face["axis"] == 0 else face["x"] + rng.randf_range(-half / 2.0 + 2.0, half / 2.0 - 2.0)
			var apos := Vector3(face["x"], 3.05, a_along) if face["axis"] == 0 else Vector3(a_along, 3.05, face["z"])
			var tilt := Basis(Vector3.UP, yaw) * Basis(Vector3.BACK, -0.32)
			awnings.append(Transform3D(tilt, apos + normal * 0.45))
	_count += pipes.size() + units.size() + awnings.size()
	_batch(pipe_mesh, pipes)
	_batch(unit_mesh, units)
	_batch(awning_mesh, awnings)

func _scatter_cables(rng: RandomNumberGenerator) -> void:
	var mat: Material = game.materials.std_mat("metal", Color(0.18, 0.19, 0.22), 0.8, 0.4)
	var seg_mesh := BoxMesh.new()
	seg_mesh.size = Vector3(1, 1, 1)
	seg_mesh.material = mat
	var segs := []
	for lane_v in LINES:
		var lane: float = lane_v
		var east := []
		var west := []
		for c in game.city.colliders:
			if c.min_x > lane + 10.0 and c.min_x < lane + 26.0 and c.h > 7.0:
				east.append(c)
			elif c.max_x < lane - 10.0 and c.max_x > lane - 26.0 and c.h > 7.0:
				west.append(c)
		var made := 0
		var tries := 0
		var want := 2 + (1 if rng.randf() < 0.5 else 0)
		while made < want and tries < 40 and not east.is_empty() and not west.is_empty():
			tries += 1
			var a: Dictionary = east[rng.randi() % east.size()]
			var b: Dictionary = west[rng.randi() % west.size()]
			var az: float = (a.min_z + a.max_z) / 2.0
			var bz: float = (b.min_z + b.max_z) / 2.0
			if absf(az - bz) > 24.0:
				continue
			var y_a: float = minf(rng.randf_range(7.0, 16.0), a.h - 1.0)
			var y_b: float = minf(rng.randf_range(7.0, 16.0), b.h - 1.0)
			var start := Vector3(a.min_x - 0.1, y_a, az + rng.randf_range(-3.0, 3.0))
			var end := Vector3(b.max_x + 0.1, y_b, bz + rng.randf_range(-3.0, 3.0))
			_add_cable_segs(start, end, rng.randf_range(0.7, 1.5), segs, 8)
			made += 1
	_batch(seg_mesh, segs)
	_count += segs.size()

func _add_cable_segs(start: Vector3, end: Vector3, sag: float, segs: Array, n: int) -> void:
	var mid := (start + end) / 2.0
	mid.y -= sag
	var prev := start
	for i in n:
		var t := (i + 1) / float(n)
		var p := _bez(start, mid, end, t)
		var dir := p - prev
		var len := dir.length()
		if len >= 0.01:
			var y_axis := dir / len
			var x_axis := Vector3.UP.cross(y_axis)
			if x_axis.length() < 0.01:
				x_axis = Vector3.RIGHT.cross(y_axis)
			x_axis = x_axis.normalized()
			var z_axis := x_axis.cross(y_axis).normalized()
			segs.append(Transform3D(Basis(x_axis * 0.03, y_axis * len, z_axis * 0.03), (prev + p) / 2.0))
		prev = p

func _bez(a: Vector3, m: Vector3, b: Vector3, t: float) -> Vector3:
	var u := 1.0 - t
	return a * (u * u) + m * (2.0 * u * t) + b * (t * t)
