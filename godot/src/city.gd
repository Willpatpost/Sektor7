extends Node3D

const CELL := 50
const GRID := 8
const HALF := 200.0

var pois := {}
var terminal := Vector3(10, 0, 131)
var colliders := []
var occupied := []
var lamp_spots := []
var rng := RandomNumberGenerator.new()
var game

var _font: FontFile
var _t := 0.0
var _signs := []
var _blinkers := []
var _traffic := []
var _rain: CPUParticles3D
var env: Environment
var _flash_light: DirectionalLight3D
var _flash := 0.0
var _next_flash := 7.0

const SIGN_WORDS := ["RAMEN", "CYBERWARE", "MOTEL", "BAR", "X-CHANGE", "TECH 24H", "CLUB", "KARAOKE", "HOTEL", "SUSHI", "VOLT", "NEON", "PAWN", "DEN", "NOODLE", "SURGE"]
const NEON := [Color(0.24, 0.95, 1.0), Color(1.0, 0.16, 0.43), Color(1.0, 0.84, 0.04), Color(0.22, 1.0, 0.53), Color(0.69, 0.15, 1.0)]

const FACADE_SRC := "
shader_type spatial;

uniform sampler2D wall_albedo : source_color, filter_linear_mipmap, repeat_enable;
uniform sampler2D wall_normal : hint_normal, filter_linear_mipmap, repeat_enable;
uniform sampler2D wall_rough : filter_linear_mipmap, repeat_enable;
uniform float tex_scale = 3.0;
uniform vec4 aux;
uniform vec2 fl;
uniform vec3 wall_col : source_color = vec3(0.7, 0.73, 0.78);
uniform float lit_ratio = 0.34;
uniform float glow_energy = 2.4;

varying vec3 wpos;
varying vec3 mnrm;
varying vec2 tuv;

float hash12(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

float vnoise(vec2 p) {
	vec2 i = floor(p);
	vec2 f = fract(p);
	f = f * f * (3.0 - 2.0 * f);
	return mix(mix(hash12(i), hash12(i + vec2(1.0, 0.0)), f.x), mix(hash12(i + vec2(0.0, 1.0)), hash12(i + vec2(1.0, 1.0)), f.x), f.y);
}

void vertex() {
	wpos = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
	mnrm = NORMAL;
	if (abs(NORMAL.y) > 0.5) {
		tuv = VERTEX.xz / tex_scale;
	} else if (abs(NORMAL.x) > 0.5) {
		tuv = vec2(VERTEX.z, VERTEX.y) / tex_scale;
	} else {
		tuv = vec2(VERTEX.x, VERTEX.y) / tex_scale;
	}
}

void fragment() {
	float concrete_n = vnoise(wpos.xz * 0.35 + wpos.y * 0.6);
	float base_rough = clamp(texture(wall_rough, tuv).r, 0.05, 1.0);
	if (abs(mnrm.y) > 0.5) {
		ALBEDO = texture(wall_albedo, tuv).rgb * wall_col * (0.6 + 0.25 * concrete_n);
		ROUGHNESS = base_rough;
		NORMAL_MAP = texture(wall_normal, tuv).rgb;
	} else {
		bool xf = abs(mnrm.x) > 0.5;
		float u0 = xf ? aux.y : aux.x;
		float pu = xf ? aux.w : aux.z;
		float u = (xf ? wpos.z : wpos.x) - u0;
		float cu = u / pu;
		float cv = wpos.y / fl.x;
		float cidu = floor(cu);
		float cidv = floor(cv);
		vec2 fuv = vec2(fract(cu), fract(cv));
		bool ground_floor = cidv < 0.5;
		float mx0 = ground_floor ? 0.07 : 0.17;
		float mx1 = ground_floor ? 0.93 : 0.83;
		float my0 = ground_floor ? 0.16 : 0.30;
		float my1 = ground_floor ? 0.94 : 0.86;
		float win = step(mx0, fuv.x) * step(fuv.x, mx1) * step(my0, fuv.y) * step(fuv.y, my1);
		vec2 cid = vec2(cidu, cidv) + fl.y;
		float h1 = hash12(cid);
		float h2 = hash12(cid + 17.31);
		float h3 = hash12(cid + 41.7);
		float lit = step(1.0 - (ground_floor ? 0.72 : lit_ratio), h1);
		float bright = 0.3 + 0.7 * h2;
		if (h3 < 0.22) {
			bright *= 0.45;
		}
		vec3 warm = vec3(1.0, 0.78, 0.47);
		vec3 cool = vec3(0.58, 0.78, 1.0);
		vec3 room = mix(warm, cool, step(0.62, h2));
		float flick = 1.0;
		if (h3 > 0.965) {
			flick = 0.55 + 0.45 * step(0.5, fract(TIME * 6.0 + h3 * 90.0));
		}
		EMISSION = room * bright * lit * glow_energy * flick * win;
		ALBEDO = texture(wall_albedo, tuv).rgb * wall_col * (0.85 + 0.3 * concrete_n);
		ROUGHNESS = mix(base_rough, 0.18, win * (1.0 - lit));
		METALLIC = win * (1.0 - lit) * 0.7;
		NORMAL_MAP = texture(wall_normal, tuv).rgb;
	}
}
"

const GROUND_SRC := "
shader_type spatial;

uniform sampler2D road_albedo : source_color, filter_linear_mipmap, repeat_enable;
uniform sampler2D road_normal : hint_normal, filter_linear_mipmap, repeat_enable;
uniform sampler2D road_rough : filter_linear_mipmap, repeat_enable;
uniform sampler2D walk_albedo : source_color, filter_linear_mipmap, repeat_enable;
uniform sampler2D walk_normal : hint_normal, filter_linear_mipmap, repeat_enable;
uniform sampler2D walk_rough : filter_linear_mipmap, repeat_enable;
uniform float tex_scale = 3.5;
uniform vec3 asphalt_tint : source_color = vec3(0.55, 0.58, 0.62);
uniform vec3 concrete_tint : source_color = vec3(0.62, 0.64, 0.68);
uniform vec3 line_col : source_color = vec3(0.95, 0.78, 0.3);
uniform float road_half = 8.0;
uniform float grid = 50.0;

varying vec3 wpos;
varying vec3 mnrm;
varying vec2 tuv;

float hash12(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

float vnoise(vec2 p) {
	vec2 i = floor(p);
	vec2 f = fract(p);
	f = f * f * (3.0 - 2.0 * f);
	return mix(mix(hash12(i), hash12(i + vec2(1.0, 0.0)), f.x), mix(hash12(i + vec2(0.0, 1.0)), hash12(i + vec2(1.0, 1.0)), f.x), f.y);
}

void vertex() {
	wpos = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
	mnrm = NORMAL;
	tuv = VERTEX.xz / tex_scale;
}

void fragment() {
	if (mnrm.y < 0.5) {
		ALBEDO = texture(road_albedo, tuv).rgb * asphalt_tint * 0.4;
		ROUGHNESS = 0.9;
	} else {
		float dx = abs(wpos.x - grid * floor(wpos.x / grid + 0.5));
		float dz = abs(wpos.z - grid * floor(wpos.z / grid + 0.5));
		float roadx = 1.0 - step(road_half, dx);
		float roadz = 1.0 - step(road_half, dz);
		float road = max(roadx, roadz);
		float inter = roadx * roadz;
		float n2 = vnoise(wpos.xz * 0.05 + 31.7);
		vec2 tile = abs(fract(wpos.xz / 2.0) - 0.5);
		float grout = smoothstep(0.44, 0.5, max(tile.x, tile.y));
		vec3 walk_alb = texture(walk_albedo, tuv).rgb * concrete_tint * mix(1.0, 0.5, grout);
		vec3 road_alb = texture(road_albedo, tuv).rgb * asphalt_tint;
		vec3 alb = mix(walk_alb, road_alb, road);
		float base_rough = mix(clamp(texture(walk_rough, tuv).r, 0.05, 1.0), clamp(texture(road_rough, tuv).r, 0.05, 1.0), road);
		float pud = smoothstep(0.62, 0.74, n2 + road * 0.06);
		ALBEDO = mix(alb, alb * 0.4, pud);
		ROUGHNESS = mix(base_rough, 0.05, pud);
		METALLIC = pud * 0.92;
		NORMAL_MAP = mix(texture(walk_normal, tuv).rgb, texture(road_normal, tuv).rgb, road);
		float center_x = (1.0 - step(0.16, dx)) * step(fract(wpos.z / 7.0), 0.55);
		float center_z = (1.0 - step(0.16, dz)) * step(fract(wpos.x / 7.0), 0.55);
		float edge_x = (1.0 - step(0.14, abs(dx - (road_half - 0.7)))) * roadx;
		float edge_z = (1.0 - step(0.14, abs(dz - (road_half - 0.7)))) * roadz;
		float mark = (center_x * roadx + center_z * roadz + edge_x + edge_z) * (1.0 - inter);
		EMISSION = line_col * mark * 0.5;
	}
}
"

const SKY_SRC := "
shader_type sky;

uniform vec3 zenith : source_color = vec3(0.008, 0.014, 0.032);
uniform vec3 horizon : source_color = vec3(0.075, 0.045, 0.105);
uniform vec3 glow_band : source_color = vec3(0.30, 0.11, 0.20);
uniform vec3 moon_dir = vec3(0.3, 0.5, -0.8);
uniform vec3 moon_col : source_color = vec3(0.75, 0.82, 1.0);

float hash12(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

void sky() {
	vec3 d = EYEDIR;
	float h = clamp(d.y, -1.0, 1.0);
	vec3 col = mix(horizon, zenith, pow(clamp(h, 0.0, 1.0), 0.38));
	col += glow_band * exp(-abs(h) * 7.5) * 0.5;
	col = mix(col, vec3(0.018, 0.022, 0.038), 1.0 - smoothstep(-0.16, -0.02, h));
	vec2 cell = floor(d.xz / (abs(d.y) + 0.22) * 72.0);
	float rnd = hash12(cell);
	float star = step(0.986, rnd) * smoothstep(0.03, 0.25, h);
	float tw = 0.55 + 0.45 * sin(TIME * 1.7 + rnd * 94.0);
	col += vec3(0.65, 0.75, 1.0) * star * tw * 0.85;
	vec3 md = normalize(moon_dir);
	float ca = dot(d, md);
	col += moon_col * pow(max(ca, 0.0), 1200.0) * 7.0;
	col += moon_col * pow(max(ca, 0.0), 20.0) * 0.14;
	COLOR = col;
}
"

const HOLO_SRC := "
shader_type spatial;
render_mode blend_add, unshaded, cull_disabled, depth_draw_never;

uniform vec3 col_a : source_color = vec3(0.24, 0.95, 1.0);
uniform vec3 col_b : source_color = vec3(0.69, 0.15, 1.0);

float hash12(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

void fragment() {
	vec2 g = floor(UV * vec2(9.0, 6.0));
	float rnd = hash12(g + floor(vec2(TIME * 0.7, TIME * 1.3)));
	float block = step(0.55, rnd);
	float scan = smoothstep(0.35, 0.5, abs(fract(UV.y * 16.0 - TIME * 1.1) - 0.5));
	float edge = smoothstep(0.0, 0.18, UV.x) * (1.0 - smoothstep(0.82, 1.0, UV.x)) * smoothstep(0.0, 0.14, UV.y) * (1.0 - smoothstep(0.86, 1.0, UV.y));
	vec3 col = mix(col_a, col_b, UV.y);
	ALBEDO = vec3(0.0);
	EMISSION = col * (0.8 + block * 1.4 + scan * 0.5) * edge;
	ALPHA = edge * (0.28 + block * 0.5);
}
"

var _facade_shader := Shader.new()
var _ground_shader := Shader.new()
var _sky_shader := Shader.new()
var _holo_shader := Shader.new()

func build(g) -> void:
	game = g
	rng.seed = 1337
	_font = FontFile.new()
	_font.load_dynamic_font("res://assets/fonts/DejaVuSans-Bold.ttf")
	_facade_shader.code = FACADE_SRC
	_ground_shader.code = GROUND_SRC
	_sky_shader.code = SKY_SRC
	_holo_shader.code = HOLO_SRC
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
	pois = {
		"spawn": Vector3(0, 0, 146),
		"plaza": Vector3(75, 0, 25),
		"lockup": Vector3(-125, 0, -16.5),
		"market": Vector3(25, 0, -75),
		"boss": Vector3(125, 0, -125)
	}
	_build_props()
	_build_traffic()
	_build_rain()
	_build_arch()
	_build_terminal()
	game.props.scatter()

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
	mi.material_override = _facade_material(x, z, w, d, h)
	body.add_child(mi)
	add_child(body)
	colliders.append({"min_x": x - w / 2.0, "max_x": x + w / 2.0, "min_z": z - d / 2.0, "max_z": z + d / 2.0, "h": h})
	if h >= 18.0:
		_roof_clutter(body, x, z, w, d, h)
	if h > 50.0:
		_glow_box(body, Vector3(x - body.position.x, h - body.position.y + 3.0, z - body.position.z), Vector3(0.4, 0.4, 0.4), Color(1.0, 0.13, 0.2))
	if rng.randf() < 0.6:
		var south := z > 0.0
		var sy := rng.randf_range(4.5, minf(h - 2.0, 20.0))
		var sz := z - d / 2.0 - 0.25 if south else z + d / 2.0 + 0.25
		_sign(x, sy, sz, PI if south else 0.0,
			SIGN_WORDS[rng.randi() % SIGN_WORDS.size()], NEON[rng.randi() % NEON.size()],
			rng.randf() < 0.3)
	if h > 40.0 and rng.randf() < 0.45:
		var bb_south := z > 0.0
		_holo(x, h * rng.randf_range(0.5, 0.72), z + (-d / 2.0 - 0.18 if bb_south else d / 2.0 + 0.18),
			minf(w * 0.85, 9.0), minf(h * 0.3, 5.0), PI if bb_south else 0.0)

func _facade_material(x: float, z: float, w: float, d: float, h: float) -> ShaderMaterial:
	var mat := ShaderMaterial.new()
	mat.shader = _facade_shader
	var p_x: float = w / maxf(1.0, roundf(w / 2.6))
	var p_z: float = d / maxf(1.0, roundf(d / 2.6))
	var n_f: int = maxi(2, int(round(h / 3.1)))
	var p_y: float = h / n_f
	mat.set_shader_parameter("aux", Vector4(x - w / 2.0, z - d / 2.0, p_x, p_z))
	mat.set_shader_parameter("fl", Vector2(p_y, rng.randf() * 97.0))
	mat.set_shader_parameter("lit_ratio", rng.randf_range(0.26, 0.42))
	mat.set_shader_parameter("wall_col", Vector3(0.58, 0.62, 0.68) * rng.randf_range(0.75, 1.15))
	mat.set_shader_parameter("tex_scale", rng.randf_range(2.4, 3.6))
	game.materials.apply(mat, "facade", "wall_")
	return mat

func _roof_clutter(parent: Node3D, x: float, z: float, w: float, d: float, h: float) -> void:
	var n := 1 + rng.randi() % 3
	var cmat: StandardMaterial3D = game.materials.std_mat("metal", Color(0.5, 0.52, 0.58), 0.7, 0.4)
	for i in n:
		var cw := rng.randf_range(1.1, 1.9)
		var chh := rng.randf_range(0.7, 1.3)
		var cd := rng.randf_range(1.0, 1.6)
		var mi := MeshInstance3D.new()
		var bm := BoxMesh.new()
		bm.size = Vector3(cw, chh, cd)
		mi.mesh = bm
		mi.position = Vector3(rng.randf_range(-w / 4.0, w / 4.0), h - parent.position.y + chh / 2.0, rng.randf_range(-d / 4.0, d / 4.0))
		mi.material_override = cmat
		parent.add_child(mi)
	if h >= 30.0 and rng.randf() < 0.8:
		var ant := MeshInstance3D.new()
		var am := CylinderMesh.new()
		am.top_radius = 0.04
		am.bottom_radius = 0.05
		am.height = rng.randf_range(3.0, 5.5)
		ant.mesh = am
		var ax := x + w / 2.0 - 0.8
		var az := z - d / 2.0 + 0.8
		ant.position = Vector3(ax - parent.position.x, h - parent.position.y + am.height / 2.0, az - parent.position.z)
		ant.material_override = game.materials.std_mat("metal", Color(0.45, 0.46, 0.5), 0.5, 0.8)
		parent.add_child(ant)
		var beacon := MeshInstance3D.new()
		var bb := BoxMesh.new()
		bb.size = Vector3(0.16, 0.16, 0.16)
		beacon.mesh = bb
		beacon.position = ant.position + Vector3(0, am.height / 2.0 + 0.1, 0)
		var bmat := StandardMaterial3D.new()
		bmat.emission_enabled = true
		bmat.emission = Color(1.0, 0.13, 0.16)
		bmat.emission_energy_multiplier = 2.0
		beacon.material_override = bmat
		parent.add_child(beacon)
		_blinkers.append({"mat": bmat, "ph": rng.randf() * 9.0})

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

func _sign(x: float, y: float, z: float, ry: float, text: String, col: Color, flicker := false) -> void:
	var root := Node3D.new()
	root.position = Vector3(x, y, z)
	root.rotation.y = ry
	var wdt := text.length() * 0.34 + 0.6
	var back := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = Vector3(wdt, 1.25, 0.12)
	back.mesh = bm
	back.position = Vector3(0, 0, -0.07)
	back.material_override = game.materials.std_mat("metal", Color(0.38, 0.4, 0.46), 0.6, 0.5)
	root.add_child(back)
	for ey in [0.58, -0.58]:
		var strip := MeshInstance3D.new()
		var sb := BoxMesh.new()
		sb.size = Vector3(wdt - 0.1, 0.07, 0.05)
		strip.mesh = sb
		strip.position = Vector3(0, ey, 0.03)
		var smat := StandardMaterial3D.new()
		smat.emission_enabled = true
		smat.emission = col
		smat.emission_energy_multiplier = 2.6
		smat.albedo_color = Color(0.03, 0.03, 0.05)
		strip.material_override = smat
		root.add_child(strip)
	var l := Label3D.new()
	l.text = text
	l.position = Vector3(0, 0, 0.06)
	l.font = _font
	l.font_size = 64
	l.outline_size = 10
	l.modulate = col * 2.2
	root.add_child(l)
	add_child(root)
	if flicker:
		_signs.append({"l": l, "base": col * 2.2, "sp": rng.randf_range(0.7, 1.4), "ph": rng.randf() * 13.0})

func _holo(x: float, y: float, z: float, w: float, h: float, ry: float) -> void:
	var mi := MeshInstance3D.new()
	var pm := PlaneMesh.new()
	pm.size = Vector2(w, h)
	mi.mesh = pm
	mi.position = Vector3(x, y, z)
	mi.rotation.y = ry
	var mat := ShaderMaterial.new()
	mat.shader = _holo_shader
	var c1: Color = NEON[rng.randi() % NEON.size()]
	var c2: Color = NEON[rng.randi() % NEON.size()]
	mat.set_shader_parameter("col_a", Vector3(c1.r, c1.g, c1.b))
	mat.set_shader_parameter("col_b", Vector3(c2.r, c2.g, c2.b))
	mi.material_override = mat
	add_child(mi)

func _build_environment() -> void:
	var we := WorldEnvironment.new()
	env = Environment.new()
	var sky := Sky.new()
	var sky_mat := ShaderMaterial.new()
	sky_mat.shader = _sky_shader
	sky.sky_material = sky_mat
	env.set("background", Environment.BG_SKY)
	env.set("tone_mapper", Environment.TONE_MAPPER_AGX)
	env.set("glow_blend_mode", Environment.GLOW_BLEND_MODE_SOFTLIGHT)
	env.set("sky", sky)
	env.set("ambient_light_source", Environment.AMBIENT_SOURCE_SKY)
	env.set("ambient_light_energy", 1.15)
	env.set("fog_enabled", true)
	env.set("fog_light_color", Color(0.05, 0.05, 0.06))
	env.set("fog_density", 0.004)
	env.set("volumetric_fog_enabled", true)
	env.set("volumetric_fog_density", 0.011)
	env.set("volumetric_fog_albedo", Color(0.55, 0.56, 0.66))
	env.set("volumetric_fog_emission", Color(0.03, 0.035, 0.06))
	env.set("volumetric_fog_emission_energy", 0.35)
	env.set("glow_enabled", true)
	env.set("glow_intensity", 1.35)
	env.set("glow_bloom", 0.12)
	env.set("glow_hdr_threshold", 1.0)
	env.set("ssao_enabled", true)
	env.set("ssao_intensity", 2.0)
	env.set("ssr_enabled", true)
	env.set("ssr_max_steps", 40)
	env.set("adjustment_enabled", true)
	env.set("adjustment_saturation", 1.07)
	env.set("adjustment_contrast", 1.05)
	we.environment = env
	add_child(we)
	var moon := DirectionalLight3D.new()
	moon.rotation = Vector3(deg_to_rad(-42), deg_to_rad(210), 0)
	moon.light_color = Color(0.62, 0.68, 0.9)
	moon.light_energy = 0.55
	moon.shadow_enabled = true
	moon.set("directional_shadow_size", 4096)
	moon.set("directional_shadow_max_distance", 90.0)
	moon.set("directional_shadow_blend_splits", true)
	moon.set("shadow_blur", 1.0)
	add_child(moon)
	sky_mat.set_shader_parameter("moon_dir", moon.global_transform.basis.z)
	_flash_light = DirectionalLight3D.new()
	_flash_light.rotation = Vector3(deg_to_rad(-70), deg_to_rad(120), 0)
	_flash_light.light_color = Color(0.85, 0.9, 1.0)
	_flash_light.light_energy = 0.0
	_flash_light.shadow_enabled = false
	add_child(_flash_light)

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
	var mat := ShaderMaterial.new()
	mat.shader = _ground_shader
	game.materials.apply(mat, "asphalt", "road_")
	game.materials.apply(mat, "sidewalk", "walk_")
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
			lamp_spots.append(lamp.position)
			_glow_box(lamp, Vector3(0, 5.6, 0), Vector3(0.12, 0.12, 1.5), Color(0.6, 0.85, 1.0))
			var pole := MeshInstance3D.new()
			var pm := BoxMesh.new()
			pm.size = Vector3(0.14, 5.6, 0.14)
			pole.mesh = pm
			pole.position = Vector3(0, 2.8, 0)
			pole.material_override = game.materials.std_mat("metal", Color(0.42, 0.44, 0.5), 0.6, 0.8)
			lamp.add_child(pole)
			var pool := MeshInstance3D.new()
			var pq := QuadMesh.new()
			pq.size = Vector2(7.0, 7.0)
			var poolmat := StandardMaterial3D.new()
			poolmat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
			poolmat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
			poolmat.cull_mode = BaseMaterial3D.CULL_DISABLED
			poolmat.albedo_color = Color(0.45, 0.62, 0.85, 0.1)
			poolmat.emission_enabled = true
			poolmat.emission = Color(0.45, 0.62, 0.85)
			poolmat.emission_energy_multiplier = 0.5
			pq.material = poolmat
			pool.mesh = pq
			pool.rotation.x = -PI / 2
			pool.position = Vector3(0, 0.02, 0)
			lamp.add_child(pool)
			if i % 2 == 0:
				var omni := OmniLight3D.new()
				omni.position = Vector3(0, 5.2, 0)
				omni.light_color = Color(0.55, 0.78, 1.0)
				omni.omni_range = 20.0
				omni.light_energy = 2.2
				lamp.add_child(omni)
			add_child(lamp)
			i += 1

func _build_props() -> void:
	for lane in [-150.0, -100.0, -50.0, 0.0, 50.0, 100.0, 150.0]:
		for k in 2:
			var side := 1.0 if rng.randf() < 0.5 else -1.0
			var z := rng.randf_range(-185.0, 185.0)
			var x: float = lane + side * 9.4
			if absf(x - terminal.x) < 2.2 and absf(z - terminal.z) < 3.0:
				continue
			_vending(x, z, side)
	for key in ["plaza", "market", "lockup"]:
		var p: Vector3 = pois[key] if pois.has(key) else Vector3(0, 0, -60)
		for j in 3:
			_barrier(p.x + [-2.8, 0.0, 2.8][j], p.z + 2.6, [0.0, 0.0, 0.0][j])

func _vending(x: float, z: float, side: float) -> void:
	occupied.append(Vector3(x, 0, z))
	var root := Node3D.new()
	root.position = Vector3(x, 0, z)
	root.rotation.y = -PI / 2.0 if side > 0.0 else PI / 2.0
	var body := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = Vector3(0.95, 1.9, 0.75)
	body.mesh = bm
	body.position = Vector3(0, 0.95, 0)
	body.material_override = game.materials.std_mat("metal", Color(0.55, 0.57, 0.62), 0.45, 0.6)
	root.add_child(body)
	var panel := MeshInstance3D.new()
	var pm := PlaneMesh.new()
	pm.size = Vector2(0.66, 1.25)
	panel.mesh = pm
	panel.position = Vector3(0, 1.1, 0.39)
	var col: Color = NEON[rng.randi() % NEON.size()]
	var pmat := StandardMaterial3D.new()
	pmat.emission_enabled = true
	pmat.emission = col
	pmat.emission_energy_multiplier = 2.6
	pmat.albedo_color = Color(0.05, 0.05, 0.08)
	panel.material_override = pmat
	root.add_child(panel)
	add_child(root)

func _barrier(x: float, z: float, ry: float) -> void:
	occupied.append(Vector3(x, 0, z))
	var root := Node3D.new()
	root.position = Vector3(x, 0, z)
	root.rotation.y = ry
	var body := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = Vector3(2.0, 0.55, 0.35)
	body.mesh = bm
	body.position = Vector3(0, 0.28, 0)
	body.material_override = game.materials.std_mat("metal", Color(0.6, 0.62, 0.66), 0.6, 0.2)
	root.add_child(body)
	var strip := MeshInstance3D.new()
	var sb := BoxMesh.new()
	sb.size = Vector3(1.9, 0.07, 0.05)
	strip.mesh = sb
	strip.position = Vector3(0, 0.58, 0)
	var smat := StandardMaterial3D.new()
	smat.emission_enabled = true
	smat.emission = Color(1.0, 0.65, 0.12)
	smat.emission_energy_multiplier = 2.2
	strip.material_override = smat
	root.add_child(strip)
	add_child(root)

func _build_traffic() -> void:
	var lines := [-150.0, -100.0, -50.0, 0.0, 50.0, 100.0, 150.0]
	for i in 14:
		var axis := i % 2
		var coord: float = lines[rng.randi() % lines.size()]
		var alt := rng.randf_range(13.0, 30.0)
		var dir := 1.0 if rng.randf() < 0.5 else -1.0
		var col: Color = NEON[rng.randi() % NEON.size()]
		var car := Node3D.new()
		_traffic_car(car, col)
		add_child(car)
		_traffic.append({"node": car, "axis": axis, "coord": coord, "alt": alt, "dir": dir,
			"spd": rng.randf_range(9.0, 15.0), "t": rng.randf_range(-200.0, 200.0), "ph": rng.randf() * 9.0})

func _traffic_car(car: Node3D, col: Color) -> void:
	var hull := MeshInstance3D.new()
	var hm := BoxMesh.new()
	hm.size = Vector3(1.7, 0.42, 3.4)
	hull.mesh = hm
	hull.position = Vector3(0, 0.5, 0)
	var hmat := StandardMaterial3D.new()
	hmat.albedo_color = Color(0.07, 0.09, 0.12)
	hmat.metallic = 0.8
	hmat.roughness = 0.35
	hull.material_override = hmat
	car.add_child(hull)
	var cabin := MeshInstance3D.new()
	var cm := BoxMesh.new()
	cm.size = Vector3(1.4, 0.38, 1.6)
	cabin.mesh = cm
	cabin.position = Vector3(0, 0.85, -0.3)
	var cmat := StandardMaterial3D.new()
	cmat.albedo_color = Color(0.03, 0.05, 0.08)
	cmat.metallic = 0.9
	cmat.roughness = 0.12
	cabin.material_override = cmat
	car.add_child(cabin)
	_traffic_glow(car, Vector3(0, 0.5, 1.72), Vector3(1.5, 0.09, 0.06), Color(0.87, 0.96, 1.0))
	_traffic_glow(car, Vector3(0, 0.55, -1.72), Vector3(1.55, 0.1, 0.06), Color(1.0, 0.13, 0.2))
	_traffic_glow(car, Vector3(0, 0.12, 0), Vector3(1.5, 0.05, 2.9), col)

func _traffic_glow(parent: Node3D, pos: Vector3, size: Vector3, col: Color) -> void:
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = size
	mi.mesh = bm
	mi.position = pos
	var mat := StandardMaterial3D.new()
	mat.emission_enabled = true
	mat.emission = col
	mat.emission_energy_multiplier = 3.0
	mat.albedo_color = Color(0.03, 0.03, 0.05)
	mi.material_override = mat
	parent.add_child(mi)

func _build_rain() -> void:
	var rain := CPUParticles3D.new()
	rain.emitting = true
	rain.amount = 800
	rain.lifetime = 1.05
	rain.emission_shape = CPUParticles3D.EMISSION_SHAPE_BOX
	rain.emission_box_extents = Vector3(34, 9, 34)
	rain.direction = Vector3(0.06, -1, 0.03)
	rain.spread = 4.0
	rain.gravity = Vector3(0, -26, 0)
	rain.initial_velocity_min = 11.0
	rain.initial_velocity_max = 15.0
	var rmesh := BoxMesh.new()
	rmesh.size = Vector3(0.025, 0.6, 0.025)
	rain.mesh = rmesh
	var rmat := StandardMaterial3D.new()
	rmat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	rmat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	rmat.albedo_color = Color(0.62, 0.72, 0.9, 0.34)
	rmat.emission_enabled = true
	rmat.emission = Color(0.25, 0.32, 0.45)
	rmat.emission_energy_multiplier = 0.6
	rmesh.material = rmat
	add_child(rain)
	_rain = rain

func _build_arch() -> void:
	occupied.append(Vector3(-7.0, 0, 150.0))
	occupied.append(Vector3(7.0, 0, 150.0))
	for s in [-7.0, 7.0]:
		var body := StaticBody3D.new()
		body.position = Vector3(s, 4.5, 150)
		var shape := CollisionShape3D.new()
		var box := BoxShape3D.new()
		box.size = Vector3(1.2, 9, 1.2)
		shape.shape = box
		body.add_child(shape)
		add_child(body)
	var beam := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = Vector3(15.2, 0.55, 0.9)
	beam.mesh = bm
	beam.position = Vector3(0, 9.4, 150)
	beam.material_override = game.materials.std_mat("metal", Color(0.48, 0.5, 0.56), 0.5, 0.7)
	add_child(beam)
	_sign(0, 9.9, 150.45, 0.0, "SEKTOR 7", Color(0.24, 0.95, 1.0))
	_sign(0, 9.9, 149.55, PI, "SEKTOR 7", Color(0.24, 0.95, 1.0))

func _build_terminal() -> void:
	occupied.append(terminal)
	var body := StaticBody3D.new()
	body.position = Vector3(10, 0.6, 131)
	var shape := CollisionShape3D.new()
	var box := BoxShape3D.new()
	box.size = Vector3(0.9, 1.2, 0.6)
	shape.shape = box
	body.add_child(shape)
	var mi := MeshInstance3D.new()
	var bm := BoxMesh.new()
	bm.size = Vector3(0.9, 1.2, 0.6)
	mi.mesh = bm
	mi.material_override = game.materials.std_mat("metal", Color(0.52, 0.55, 0.62), 0.45, 0.5)
	body.add_child(mi)
	var screen := MeshInstance3D.new()
	var sb := BoxMesh.new()
	sb.size = Vector3(0.7, 0.5, 0.08)
	screen.mesh = sb
	screen.position = Vector3(0, 0.55, 0.2)
	screen.rotation.x = -0.3
	var smat := StandardMaterial3D.new()
	smat.emission_enabled = true
	smat.emission = Color(0.24, 0.95, 1.0)
	smat.emission_energy_multiplier = 2.2
	screen.material_override = smat
	body.add_child(screen)
	var awning := MeshInstance3D.new()
	var ab := BoxMesh.new()
	ab.size = Vector3(1.3, 0.06, 0.7)
	awning.mesh = ab
	awning.position = Vector3(0, 1.28, 0.25)
	var amat := StandardMaterial3D.new()
	amat.emission_enabled = true
	amat.emission = Color(1.0, 0.16, 0.43)
	amat.emission_energy_multiplier = 1.6
	amat.albedo_color = Color(0.05, 0.05, 0.07)
	awning.material_override = amat
	body.add_child(awning)
	add_child(body)
	_sign(10, 3.4, 131.4, 0.0, "RIPPERDOC", Color(1.0, 0.16, 0.43), true)

func _process(delta: float) -> void:
	_t += delta
	for e in _signs:
		var l: Label3D = e["l"]
		var ft := fmod(_t * float(e["sp"]) + float(e["ph"]), 13.0)
		var m := 1.0
		if ft < 0.85:
			m = 0.25 + 0.12 * sin(_t * 43.0)
		l.modulate = e["base"] * m
	for b in _blinkers:
		var bm: StandardMaterial3D = b["mat"]
		bm.emission_energy_multiplier = 1.1 + 1.3 * (0.5 + 0.5 * sin(_t * 2.2 + float(b["ph"])))
	for c in _traffic:
		var along: float = float(c["t"]) + float(c["spd"]) * delta * float(c["dir"])
		if along > 235.0:
			along = -235.0
		elif along < -235.0:
			along = 235.0
		c["t"] = along
		var weave := sin(_t * 0.6 + float(c["ph"])) * 1.3
		var bob := sin(_t * 0.85 + float(c["ph"]) * 2.0) * 0.5
		var bank := -cos(_t * 0.6 + float(c["ph"])) * 0.06
		var n: Node3D = c["node"]
		if int(c["axis"]) == 0:
			n.position = Vector3(along, float(c["alt"]) + bob, float(c["coord"]) + weave)
			n.rotation = Vector3(0.0, PI / 2.0 if float(c["dir"]) > 0.0 else -PI / 2.0, bank)
		else:
			n.position = Vector3(float(c["coord"]) + weave, float(c["alt"]) + bob, along)
			n.rotation = Vector3(0.0, 0.0 if float(c["dir"]) > 0.0 else PI, bank)
	if game != null and game.cam != null and _rain != null:
		_rain.position = game.cam.global_position + Vector3(0.0, 7.0, 0.0)
	if _flash_light != null:
		_flash -= delta
		if _flash > 0.0:
			_flash_light.light_energy = 5.0 * pow(clampf(_flash / 0.16, 0.0, 1.0), 1.5)
		else:
			_flash_light.light_energy = 0.0
			_next_flash -= delta
			if _next_flash <= 0.0:
				_flash = 0.16
				_next_flash = rng.randf_range(9.0, 22.0)
