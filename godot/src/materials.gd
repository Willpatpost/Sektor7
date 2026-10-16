extends Node

## PBR texture-set registry and shared material library.
##
## A texture set is a directory containing albedo.jpg / normal.jpg / roughness.jpg
## (see assets/textures/CREDITS.md). Sets are registered below, loaded once, and
## handed out two ways:
##   apply(shader_material, key, prefix) - sets <prefix>albedo/normal/roughness
##       sampler uniforms on a ShaderMaterial (city surface shaders)
##   std_mat(key, tint, rough, metal)   - cached StandardMaterial3D for props
## Missing files degrade to generated flat maps so the game always runs.

var game
var _sets := {}
var _std_cache := {}
var _real_sets := 0

const MAPS := ["albedo", "normal", "roughness"]

const REGISTRY := {
	"asphalt": "res://assets/textures/roads/asphalt_02",
	"sidewalk": "res://assets/textures/sidewalks/concrete_floor_02",
	"facade": "res://assets/textures/facades/concrete_panels",
	"metal": "res://assets/textures/metal/metal_plate",
}

func setup(g) -> void:
	game = g
	for key in REGISTRY:
		_load_set(key, REGISTRY[key])

func _load_set(key: String, dir_path: String) -> void:
	var maps := {}
	var complete := true
	for m in MAPS:
		var p: String = dir_path + "/" + m + ".jpg"
		if ResourceLoader.exists(p):
			maps[m] = load(p)
		else:
			maps[m] = _fallback(m)
			complete = false
	_sets[key] = maps
	if complete:
		_real_sets += 1

func _fallback(kind: String) -> ImageTexture:
	var col := Color(0.5, 0.5, 0.5)
	if kind == "normal":
		col = Color(0.5, 0.5, 1.0)
	var img := Image.create_empty(4, 4, false, Image.FORMAT_RGB8)
	img.fill(col)
	return ImageTexture.create_from_image(img)

func set_count() -> int:
	return _real_sets

func tex(key: String) -> Dictionary:
	return _sets.get(key, {})

func apply(mat: ShaderMaterial, key: String, prefix := "") -> void:
	var maps: Dictionary = tex(key)
	for m in MAPS:
		mat.set_shader_parameter(prefix + m, maps[m])

func std_mat(key: String, tint := Color(0.8, 0.8, 0.8), rough := 1.0, metal := 0.0) -> StandardMaterial3D:
	var ck := "%s|%s|%f|%f" % [key, tint.to_html(), rough, metal]
	if _std_cache.has(ck):
		return _std_cache[ck]
	var maps: Dictionary = tex(key)
	var mat := StandardMaterial3D.new()
	if maps.has("albedo"):
		mat.albedo_texture = maps["albedo"]
	if maps.has("normal"):
		mat.normal_enabled = true
		mat.normal_texture = maps["normal"]
	if maps.has("roughness"):
		mat.roughness_texture = maps["roughness"]
	mat.albedo_color = tint
	mat.roughness = rough
	mat.metallic = metal
	_std_cache[ck] = mat
	return mat
