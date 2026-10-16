extends CanvasLayer

var game
var main_root: Control
var pause_root: Control
var settings_root: Control
var howto_root: Control
var tab_audio: VBoxContainer
var tab_video: VBoxContainer
var tab_controls: VBoxContainer
var btn_main_menu: Button
var _bind_buttons := {}
var _tab_btns := {}
var _capture_action := ""
var _capture_hint: Label
var _from := "pause"

const BUS := {"master": "Master", "music": "Music", "sfx": "SFX"}

const PAD_NAMES := {
	JOY_BUTTON_A: "PAD A", JOY_BUTTON_B: "PAD B", JOY_BUTTON_X: "PAD X", JOY_BUTTON_Y: "PAD Y",
	JOY_BUTTON_LEFT_SHOULDER: "PAD LB", JOY_BUTTON_RIGHT_SHOULDER: "PAD RB",
	JOY_BUTTON_LEFT_STICK: "PAD L3", JOY_BUTTON_RIGHT_STICK: "PAD R3",
	JOY_BUTTON_BACK: "PAD SELECT", JOY_BUTTON_START: "PAD START",
	JOY_BUTTON_DPAD_UP: "PAD UP", JOY_BUTTON_DPAD_DOWN: "PAD DOWN",
	JOY_BUTTON_DPAD_LEFT: "PAD LEFT", JOY_BUTTON_DPAD_RIGHT: "PAD RIGHT"
}

const REMAP := [
	["move_forward", "MOVE FORWARD"], ["move_back", "MOVE BACK"],
	["move_left", "STRAFE LEFT"], ["move_right", "STRAFE RIGHT"],
	["sprint", "SPRINT"], ["jump", "JUMP / FLY UP"], ["fly_down", "FLY DOWN"],
	["interact", "INTERACT"], ["reload", "RELOAD"],
	["weapon_1", "WEAPON 1"], ["weapon_2", "WEAPON 2"], ["pov_cycle", "CYCLE POV"]
]

func setup(g) -> void:
	game = g
	layer = 20
	_build_main()
	_build_pause()
	_build_settings()
	_build_howto()
	close_all()

func open_main() -> void:
	close_all()
	main_root.visible = true

func open_pause() -> void:
	close_all()
	pause_root.visible = true

func close_all() -> void:
	main_root.visible = false
	pause_root.visible = false
	settings_root.visible = false
	howto_root.visible = false
	_capture_action = ""

func open_settings_from(from: String) -> void:
	_from = from
	close_all()
	settings_root.visible = true
	_show_tab("audio")

func open_howto_from(from: String) -> void:
	_from = from
	close_all()
	howto_root.visible = true

func consume_esc() -> bool:
	if settings_root.visible or howto_root.visible:
		settings_root.visible = false
		howto_root.visible = false
		if _from == "main":
			main_root.visible = true
		else:
			pause_root.visible = true
		return true
	return false

func _input(event: InputEvent) -> void:
	if _capture_action != "":
		if event is InputEventKey and event.pressed and not event.echo:
			if event.keycode == KEY_ESCAPE:
				_cancel_capture()
			else:
				_commit_remap(_capture_action, event.keycode)
			get_viewport().set_input_as_handled()
		elif event is InputEventJoypadButton and event.pressed:
			_commit_remap_pad(_capture_action, event.button_index)
			get_viewport().set_input_as_handled()
		return
	if event is InputEventKey and event.pressed and not event.echo and event.keycode == KEY_ESCAPE and game.state == "paused":
		if not consume_esc():
			game.resume_game()
		get_viewport().set_input_as_handled()

func _cancel_capture() -> void:
	_capture_action = ""
	_capture_hint.text = ""

func _commit_remap(action: String, kc: int) -> void:
	game._replace_key(action, kc)
	_save_binds()
	_refresh_binds()
	_cancel_capture()

func _commit_remap_pad(action: String, btn: int) -> void:
	game._replace_pad(action, btn)
	_save_binds()
	_refresh_binds()
	_cancel_capture()

func _save_binds() -> void:
	var cfg := ConfigFile.new()
	cfg.load("user://settings.cfg")
	for pair in REMAP:
		var action: String = pair[0]
		for ev in InputMap.action_get_events(action):
			if ev is InputEventKey:
				cfg.set_value("binds", action, ev.keycode)
			elif ev is InputEventJoypadButton:
				cfg.set_value("pad_binds", action, ev.button_index)
	cfg.save("user://settings.cfg")

func _refresh_binds() -> void:
	for action in _bind_buttons:
		_bind_buttons[action].text = _bind_label(action)

func _bind_label(action: String) -> String:
	var parts: PackedStringArray = []
	for ev in InputMap.action_get_events(action):
		if ev is InputEventKey:
			parts.append(OS.get_keycode_string(ev.keycode))
		elif ev is InputEventJoypadButton:
			var idx: int = ev.button_index
			parts.append(str(PAD_NAMES.get(idx, "PAD %d" % idx)))
	if parts.is_empty():
		return "(none)"
	return " / ".join(parts)

func set_fov(v: float) -> void:
	game.settings["fov"] = v
	game.cam.fov = v
	game.save_settings()

func set_brightness(v: float) -> void:
	game.settings["brightness"] = v
	if game.city != null and game.city.env != null:
		game.city.env.set("adjustment_brightness", v)
	game.save_settings()

func set_volume(key: String, v: float) -> void:
	game.settings[key] = v
	var bus: String = BUS[key]
	AudioServer.set_bus_volume_linear(AudioServer.get_bus_index(bus), v)
	game.save_settings()

func set_sens(key: String, v: float) -> void:
	game.settings[key] = v
	game.save_settings()

func _build_main() -> void:
	main_root = _root_ctrl()
	var panel := _centered_panel(main_root, 430.0)
	var vb := VBoxContainer.new()
	vb.add_theme_constant_override("separation", 10)
	panel.add_child(vb)
	var title := Label.new()
	title.text = "SEKTOR 7"
	title.add_theme_font_size_override("font_size", 64)
	title.add_theme_color_override("font_color", Color(0.24, 0.95, 1.0))
	title.add_theme_constant_override("outline_size", 10)
	title.add_theme_color_override("font_outline_color", Color(0.24, 0.95, 1.0, 0.22))
	title.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	vb.add_child(title)
	var sub := _mk_label("NIGHT CITY — GODOT BUILD", 13, Color(0.5, 0.62, 0.72))
	sub.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	vb.add_child(sub)
	vb.add_child(_spacer(16))
	_mk_btn(vb, "NEW RUN", func(): game.start_game())
	_mk_btn(vb, "SETTINGS", func(): open_settings_from("main"))
	_mk_btn(vb, "HOW TO PLAY", func(): open_howto_from("main"))
	_mk_btn(vb, "QUIT", func(): get_tree().quit())

func _build_pause() -> void:
	pause_root = _root_ctrl()
	var panel := _centered_panel(pause_root, 430.0)
	var vb := VBoxContainer.new()
	vb.add_theme_constant_override("separation", 10)
	panel.add_child(vb)
	var title := _mk_label("PAUSED", 30, Color(0.24, 0.95, 1.0))
	title.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	vb.add_child(title)
	vb.add_child(_spacer(10))
	_mk_btn(vb, "RESUME", func(): game.resume_game())
	_mk_btn(vb, "SETTINGS", func(): open_settings_from("pause"))
	_mk_btn(vb, "HOW TO PLAY", func(): open_howto_from("pause"))
	btn_main_menu = _mk_btn(vb, "MAIN MENU", func(): game.return_to_main_menu())

func _build_settings() -> void:
	settings_root = _root_ctrl()
	var panel := _centered_panel(settings_root, 680.0)
	var vb := VBoxContainer.new()
	vb.add_theme_constant_override("separation", 8)
	panel.add_child(vb)
	var title := _mk_label("SETTINGS  —  [ESC] BACK", 24, Color(0.24, 0.95, 1.0))
	title.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	vb.add_child(title)
	var tabs := HBoxContainer.new()
	tabs.add_theme_constant_override("separation", 8)
	tabs.alignment = BoxContainer.ALIGNMENT_CENTER
	vb.add_child(tabs)
	for pair in [["audio", "AUDIO"], ["video", "VIDEO"], ["controls", "CONTROLS"]]:
		var name_: String = pair[0]
		var b := Button.new()
		b.text = pair[1]
		b.toggle_mode = true
		b.add_theme_font_size_override("font_size", 14)
		b.custom_minimum_size = Vector2(140, 34)
		b.pressed.connect(func(): _show_tab(name_))
		tabs.add_child(b)
		_tab_btns[name_] = b
	tab_audio = VBoxContainer.new()
	tab_audio.add_theme_constant_override("separation", 10)
	vb.add_child(tab_audio)
	tab_video = VBoxContainer.new()
	tab_video.add_theme_constant_override("separation", 10)
	vb.add_child(tab_video)
	tab_controls = VBoxContainer.new()
	tab_controls.add_theme_constant_override("separation", 6)
	vb.add_child(tab_controls)
	_mk_slider(tab_audio, "MASTER VOLUME", 0.0, 1.0, 0.01, float(game.settings["master"]),
		func(v: float): set_volume("master", v))
	_mk_slider(tab_audio, "MUSIC", 0.0, 1.0, 0.01, float(game.settings["music"]),
		func(v: float): set_volume("music", v))
	_mk_slider(tab_audio, "SOUND EFFECTS", 0.0, 1.0, 0.01, float(game.settings["sfx"]),
		func(v: float): set_volume("sfx", v))
	_mk_slider(tab_video, "FIELD OF VIEW (POV WIDTH)", 60.0, 110.0, 1.0, float(game.settings["fov"]),
		func(v: float): set_fov(v))
	_mk_slider(tab_video, "BRIGHTNESS", 0.5, 1.6, 0.05, float(game.settings["brightness"]),
		func(v: float): set_brightness(v))
	var wrow := HBoxContainer.new()
	wrow.add_theme_constant_override("separation", 12)
	var wl := _mk_label("WINDOW MODE", 14, Color(0.72, 0.8, 0.88))
	wl.custom_minimum_size = Vector2(220, 0)
	wrow.add_child(wl)
	var ob := OptionButton.new()
	ob.add_theme_font_size_override("font_size", 14)
	ob.add_item("WINDOWED")
	ob.add_item("FULLSCREEN")
	ob.add_item("BORDERLESS FULLSCREEN")
	ob.select(int(game.settings["window_mode"]))
	ob.item_selected.connect(func(i: int):
		game.settings["window_mode"] = i
		game._apply_window_mode(i)
		game.save_settings())
	wrow.add_child(ob)
	tab_video.add_child(wrow)
	_mk_slider(tab_controls, "MOUSE SENSITIVITY", 0.2, 3.0, 0.05, float(game.settings["mouse_sens"]),
		func(v: float): set_sens("mouse_sens", v))
	_mk_slider(tab_controls, "STICK SENSITIVITY", 0.2, 3.0, 0.05, float(game.settings["stick_sens"]),
		func(v: float): set_sens("stick_sens", v))
	var scroll := ScrollContainer.new()
	scroll.custom_minimum_size = Vector2(620, 300)
	tab_controls.add_child(scroll)
	var list := VBoxContainer.new()
	list.add_theme_constant_override("separation", 4)
	scroll.add_child(list)
	var size_flag := Control.SIZE_EXPAND_FILL
	list.size_flags_horizontal = size_flag
	for pair in REMAP:
		var action: String = pair[0]
		var row := HBoxContainer.new()
		row.add_theme_constant_override("separation", 10)
		var l := _mk_label(pair[1], 13, Color(0.72, 0.8, 0.88))
		l.custom_minimum_size = Vector2(210, 0)
		row.add_child(l)
		var b := Button.new()
		b.text = _bind_label(action)
		b.custom_minimum_size = Vector2(230, 32)
		b.add_theme_font_size_override("font_size", 13)
		b.pressed.connect(func(): _start_capture(action))
		row.add_child(b)
		_bind_buttons[action] = b
		list.add_child(row)
	_capture_hint = _mk_label("", 13, Color(1.0, 0.84, 0.04))
	tab_controls.add_child(_capture_hint)

func _build_howto() -> void:
	howto_root = _root_ctrl()
	var panel := _centered_panel(howto_root, 660.0)
	var vb := VBoxContainer.new()
	vb.add_theme_constant_override("separation", 8)
	panel.add_child(vb)
	var title := _mk_label("HOW TO PLAY  —  [ESC] BACK", 24, Color(0.24, 0.95, 1.0))
	title.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	vb.add_child(title)
	var body := Label.new()
	body.text = "You are a street runner in Sektor 7. Answer VEX's calls, work the jobs, and take down RAZORTAIL to clear the city.\n\nMOVE      WASD / left stick      SPRINT      SHIFT / L3\nLOOK      mouse / right stick      FIRE      LMB / RT\nJUMP / FLY UP      SPACE / A      FLY DOWN      C / LB\nINTERACT / DIALOGUE / BOARDS      E / B\nRELOAD      R / X      WEAPONS      1, 2 / Y\nCAMERA      V cycles 5 POVs      MENU      ESC\n\nE near a hovercar boards it — SPACE climbs, C descends, WASD flies. Land on rooftops.\nEarnings buy chrome at the RIPPERDOC terminal. Rebind everything in SETTINGS - CONTROLS."
	body.add_theme_font_size_override("font_size", 15)
	body.add_theme_color_override("font_color", Color(0.78, 0.85, 0.92))
	body.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	body.custom_minimum_size = Vector2(600, 330)
	vb.add_child(body)

func _start_capture(action: String) -> void:
	_capture_action = action
	_capture_hint.text = "PRESS A KEY OR PAD BUTTON — ESC CANCELS"

func _show_tab(name: String) -> void:
	tab_audio.visible = name == "audio"
	tab_video.visible = name == "video"
	tab_controls.visible = name == "controls"
	for k in _tab_btns:
		_tab_btns[k].set_pressed_no_signal(k == name)

func _root_ctrl() -> Control:
	var c := Control.new()
	c.set_anchors_preset(Control.PRESET_FULL_RECT)
	c.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(c)
	return c

func _panel(w: float) -> PanelContainer:
	var p := PanelContainer.new()
	var sb := StyleBoxFlat.new()
	sb.bg_color = Color(0.02, 0.03, 0.055, 0.92)
	sb.border_color = Color(0.24, 0.95, 1.0, 0.4)
	sb.set_border_width_all(1)
	sb.set_corner_radius_all(6)
	sb.content_margin_left = 24.0
	sb.content_margin_right = 24.0
	sb.content_margin_top = 22.0
	sb.content_margin_bottom = 22.0
	p.add_theme_stylebox_override("panel", sb)
	p.custom_minimum_size = Vector2(w, 0)
	return p

func _centered_panel(root: Control, w: float) -> PanelContainer:
	var cc := CenterContainer.new()
	cc.set_anchors_preset(Control.PRESET_FULL_RECT)
	cc.mouse_filter = Control.MOUSE_FILTER_IGNORE
	root.add_child(cc)
	var p := _panel(w)
	cc.add_child(p)
	return p

func _mk_btn(parent: Control, text: String, cb: Callable) -> Button:
	var b := Button.new()
	b.text = text
	b.custom_minimum_size = Vector2(0, 42)
	b.add_theme_font_size_override("font_size", 17)
	b.add_theme_color_override("font_color", Color(0.75, 0.85, 0.95))
	b.add_theme_color_override("font_hover_color", Color(0.24, 0.95, 1.0))
	b.add_theme_color_override("font_pressed_color", Color(0.24, 0.95, 1.0))
	var n := StyleBoxFlat.new()
	n.bg_color = Color(0.05, 0.08, 0.12, 0.85)
	n.set_corner_radius_all(3)
	var h := StyleBoxFlat.new()
	h.bg_color = Color(0.1, 0.22, 0.3, 0.9)
	h.set_corner_radius_all(3)
	b.add_theme_stylebox_override("normal", n)
	b.add_theme_stylebox_override("hover", h)
	b.add_theme_stylebox_override("pressed", h)
	b.pressed.connect(cb)
	parent.add_child(b)
	return b

func _mk_label(text: String, size: int, col: Color) -> Label:
	var l := Label.new()
	l.text = text
	l.add_theme_font_size_override("font_size", size)
	l.add_theme_color_override("font_color", col)
	return l

func _mk_slider(parent: Control, text: String, minv: float, maxv: float, step: float, val: float, cb: Callable) -> HSlider:
	var row := HBoxContainer.new()
	row.add_theme_constant_override("separation", 12)
	var l := _mk_label(text, 14, Color(0.72, 0.8, 0.88))
	l.custom_minimum_size = Vector2(220, 0)
	row.add_child(l)
	var s := HSlider.new()
	s.min_value = minv
	s.max_value = maxv
	s.step = step
	s.value = val
	s.custom_minimum_size = Vector2(250, 22)
	s.value_changed.connect(cb)
	row.add_child(s)
	var v := _mk_label("%.2f" % val, 14, Color(0.24, 0.95, 1.0))
	v.custom_minimum_size = Vector2(52, 0)
	s.value_changed.connect(func(x: float):
		v.text = ("%d" % int(x)) if step >= 1.0 else ("%.2f" % x))
	row.add_child(v)
	parent.add_child(row)
	return s

func _spacer(h: float) -> Control:
	var c := Control.new()
	c.custom_minimum_size = Vector2(0, h)
	return c
