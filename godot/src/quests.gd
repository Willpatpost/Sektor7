extends Node3D

var game
var idx := -1
var active := false
var progress := 0
var all_done := false
var pending := {}
var shard: Area3D = null
var marker: Node3D = null

const JOBS := [
	{
		"title": "WET WORK", "marker": "plaza", "kind": "kill", "target": "merc", "count": 4,
		"objective": "Neutralize the Rogue-Signal mercs holding KANEDA PLAZA",
		"reward": {"eddies": 400, "xp": 200},
		"brief": [
			"V. Good — you picked up. Call me VEX. I broker work in Sektor 7.",
			"Rogue-Signal mercs just tagged KANEDA PLAZA like they own the district.",
			"Four of them. Loud, ugly, armed.",
			"Clear the plaza. ¢400 says nobody misses them.",
			"One more thing — see DOC CHROME. His terminal is on your block. Kit up."],
		"done": [
			"Plaza breathes again. ¢400 wired to your account.",
			"Those mercs took contracts from HELIX DYNAMICS. A corp.",
			"HELIX just noticed you exist. Wait for my ping."]
	},
	{
		"title": "COLD STORAGE", "marker": "lockup", "kind": "shard",
		"objective": "Recover the HELIX data shard from the lockup",
		"reward": {"eddies": 500, "xp": 250},
		"brief": [
			"A HELIX transport dumped a data shard into the old lockup, northwest side.",
			"It is still in the crate by the loading door.",
			"They posted a security detail. Light them up if they blink at you.",
			"Bring me the shard. ¢500."],
		"done": [
			"Shard reads clean... V, this thing is a roster. Every merc in the district.",
			"HELIX knows what they lost. They are watching the skies now."]
	},
	{
		"title": "AIR FREIGHT", "marker": "market", "kind": "kill", "target": "drone", "count": 4,
		"objective": "Down the HELIX recon drones over MOTO MARKET",
		"reward": {"eddies": 600, "xp": 300},
		"brief": [
			"HELIX drones are circling MOTO MARKET, mapping faces.",
			"Four of them, up high. They burn easy.",
			"¢600. Keep the sky clean, V."],
		"done": [
			"Market is dark to their optics now.",
			"The merc captain goes by RAZORTAIL. She parked her crew in the east plaza.",
			"Last job, V. The big one."]
	},
	{
		"title": "SEKTOR 7", "marker": "boss", "kind": "kill", "target": "boss", "count": 1,
		"objective": "Eliminate RAZORTAIL — the Rogue-Signal captain",
		"reward": {"eddies": 1000, "xp": 500},
		"brief": [
			"RAZORTAIL runs Rogue-Signal's whole Sektor 7 operation.",
			"Two bodyguards. Her? Six hundred pounds of chrome and bad decisions.",
			"Take her off the board and this district remembers your name.",
			"¢1000. Make it loud."],
		"done": [
			"Razortail's flatline is all over the nets already.",
			"SEKTOR 7 CLEARED, V. You are somebody now."]
	}
]

func setup(g) -> void:
	game = g
	marker = Node3D.new()
	var dia := MeshInstance3D.new()
	var om := PrismMesh.new()
	om.size = Vector3(1.0, 1.1, 1.0)
	dia.mesh = om
	var dmat := StandardMaterial3D.new()
	dmat.emission_enabled = true
	dmat.emission = Color(1.0, 0.84, 0.04)
	dmat.emission_energy_multiplier = 2.0
	dia.position.y = 3.0
	marker.add_child(dia)
	var beam := MeshInstance3D.new()
	var bb := BoxMesh.new()
	bb.size = Vector3(0.5, 40, 0.5)
	beam.mesh = bb
	var bmat := StandardMaterial3D.new()
	bmat.emission_enabled = true
	bmat.emission = Color(1.0, 0.84, 0.04)
	bmat.emission_energy_multiplier = 0.5
	bmat.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	bmat.albedo_color = Color(1, 0.84, 0.04, 0.14)
	beam.position.y = 20.0
	marker.add_child(beam)
	marker.visible = false
	add_child(marker)

func begin(i: int) -> void:
	idx = i
	progress = 0
	active = true
	var job: Dictionary = JOBS[i]
	game.hud.dialogue("VEX", job["brief"])
	_spawn_job(job)
	game.audio.sfx("quest")

func _spawn_job(job: Dictionary) -> void:
	var poi: Vector3 = game.city.pois[job["marker"]]
	match job["marker"]:
		"plaza":
			for i in int(job["count"]):
				game.enemies.spawn_merc(poi.x + (i - 1.5) * 4.0, poi.z, true)
		"lockup":
			for i in 3:
				game.enemies.spawn_merc(poi.x + (i - 1.0) * 5.0, poi.z - 6.0, true)
			game.enemies.spawn_drone(poi.x, poi.z - 10.0)
			_spawn_shard(poi.x, poi.z + 0.6)
		"market":
			for i in int(job["count"]):
				game.enemies.spawn_drone(poi.x + (i - 1.5) * 5.0, poi.z)
		"boss":
			game.enemies.spawn_boss(poi.x, poi.z)
			for i in 2:
				game.enemies.spawn_merc(poi.x + (i - 0.5) * 6.0, poi.z + 4.0, true)

func _spawn_shard(x: float, z: float) -> void:
	var area := Area3D.new()
	var cs := CollisionShape3D.new()
	var ss := SphereShape3D.new()
	ss.radius = 1.7
	cs.shape = ss
	area.add_child(cs)
	var mi := MeshInstance3D.new()
	var om := PrismMesh.new()
	om.size = Vector3(0.55, 0.6, 0.55)
	mi.mesh = om
	var mat := StandardMaterial3D.new()
	mat.emission_enabled = true
	mat.emission = Color(1.0, 0.84, 0.04)
	mat.emission_energy_multiplier = 2.2
	mi.material_override = mat
	area.add_child(mi)
	area.position = Vector3(x, 1.0, z)
	area.body_entered.connect(_on_shard_body)
	add_child(area)
	shard = area

func _on_shard_body(body: Node3D) -> void:
	if body == game.player and active and idx >= 0 and JOBS[idx]["kind"] == "shard":
		collect_shard()

func collect_shard() -> void:
	if not active or idx < 0 or JOBS[idx]["kind"] != "shard":
		return
	if shard != null:
		shard.queue_free()
		shard = null
	complete()

func on_kill(kind: String) -> void:
	if not active or idx < 0:
		return
	var job: Dictionary = JOBS[idx]
	if job["kind"] == "kill" and job["target"] == kind:
		progress += 1
		if progress >= int(job["count"]):
			complete()

func complete() -> void:
	active = false
	var job: Dictionary = JOBS[idx]
	game.add_eddies(int(job["reward"]["eddies"]))
	game.player.add_xp(int(job["reward"]["xp"]))
	game.hud.toast("JOB COMPLETE  +¢%d" % int(job["reward"]["eddies"]), Color(1.0, 0.84, 0.04))
	game.audio.sfx("quest")
	if idx == 0:
		game.weapons.unlock_smg()
	var next_idx := idx
	var cb := func() -> void:
		if next_idx < JOBS.size() - 1:
			pending = {"next": next_idx + 1, "t": 4.0}
		else:
			all_done = true
			game.win()
	game.hud.dialogue("VEX", job["done"], cb)

func _physics_process(delta: float) -> void:
	if game == null or game.state != "playing":
		return
	if not pending.is_empty():
		pending["t"] = float(pending["t"]) - delta
		if float(pending["t"]) <= 0.0:
			var n := int(pending["next"])
			pending = {}
			begin(n)
	if shard != null:
		shard.rotate_y(delta * 2.0)
		shard.position.y = 1.0 + sin(game.time * 2.4) * 0.15
	var mp = marker_pos()
	marker.visible = mp != null
	if mp != null:
		marker.position = Vector3(mp.x, 0, mp.z)
		marker.rotate_y(delta * 1.5)

func marker_pos():
	if not active:
		return null
	return game.city.pois[JOBS[idx]["marker"]]

func objective_info() -> Dictionary:
	if all_done:
		return {"title": "SEKTOR 7", "task": "District cleared — free roam", "prog": ""}
	if not active:
		if not pending.is_empty():
			return {"title": "STANDBY", "task": "Awaiting VEX...", "prog": ""}
		return {"title": "STANDBY", "task": "Awaiting contact...", "prog": ""}
	var job: Dictionary = JOBS[idx]
	var prog := ""
	if job["kind"] == "kill":
		prog = "TARGETS DOWN %d / %d" % [progress, int(job["count"])]
	elif job["kind"] == "shard":
		prog = "RECOVER THE SHARD"
	return {"title": "JOB %d — %s" % [idx + 1, job["title"]], "task": job["objective"], "prog": prog}
