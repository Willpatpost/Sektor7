extends SceneTree

var _done := false

func _initialize() -> void:
	call_deferred("_run")

func _arg(name: String, def: String) -> String:
	for a in OS.get_cmdline_user_args():
		if a.begins_with("--" + name + "="):
			return a.split("=")[1]
	return def

func _run() -> void:
	var out := _arg("out", "/tmp/shot.png")
	var game: Node3D = load("res://main.tscn").instantiate()
	root.add_child(game)
	for i in 10:
		await process_frame
	game.state = "playing"
	var p: CharacterBody3D = game.player
	p.position = Vector3(float(_arg("x", "0")), float(_arg("y", "0")), float(_arg("z", "146")))
	p.velocity = Vector3.ZERO
	p.rotation.y = deg_to_rad(float(_arg("yaw", "0")))
	p.head.rotation.x = deg_to_rad(float(_arg("pitch", "-4")))
	match _arg("pov", "first"):
		"behind_close": game.pov.idx = 1
		"behind_far": game.pov.idx = 2
		"front_close": game.pov.idx = 3
		"front_far": game.pov.idx = 4
	if _arg("hud", "1") == "0":
		game.hud.visible = false
	for i in int(_arg("frames", "12")):
		await process_frame
	var img: Image = null
	var vt := root.get_texture()
	if vt != null:
		img = vt.get_image()
	if img == null:
		print("SHOT FAILED - no viewport texture")
		_done = true
		game.free()
		quit(1)
		return
	img.save_png(out)
	print("SHOT SAVED ", out, " ", img.get_size().x, "x", img.get_size().y)
	_done = true
	game.free()
	quit(0)
