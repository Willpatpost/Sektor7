extends Node

var game
var engine_player: AudioStreamPlayer

func setup(g) -> void:
	game = g
	engine_player = AudioStreamPlayer.new()
	engine_player.stream = _engine_loop()
	engine_player.volume_db = -14.0
	engine_player.set("bus", "SFX")
	add_child(engine_player)

func _pcm(samples: int, gen: Callable) -> AudioStreamWAV:
	var data := PackedByteArray()
	data.resize(samples * 2)
	for i in samples:
		var v: float = clampf(gen.call(i / 44100.0, i), -1.0, 1.0)
		data.encode_s16(i * 2, int(v * 32000.0))
	var wav := AudioStreamWAV.new()
	wav.data = data
	wav.format = AudioStreamWAV.FORMAT_16_BITS
	wav.mix_rate = 44100
	wav.stereo = false
	return wav

func sfx(name: String, vol := 1.0) -> void:
	var wav: AudioStreamWAV = null
	if name == "shoot":
		wav = _pcm(int(44100 * 0.09), func(t: float, _i: int) -> float:
			var decay := 1.0 - t / 0.09
			return (sin(t * 1130.0 * (1.0 - t * 5.0)) * 0.5 + sin(t * 310.0) * 0.5) * decay * 0.7)
	elif name == "hit":
		wav = _pcm(int(44100 * 0.04), func(t: float, _i: int) -> float:
			return sin(t * 7850.0) * 0.3 * (1.0 - t / 0.04))
	elif name == "pickup":
		wav = _pcm(int(44100 * 0.16), func(t: float, _i: int) -> float:
			return sin(t * 6.283 * (780.0 + t * 5000.0)) * 0.25 * (1.0 - t / 0.16))
	elif name == "hurt":
		wav = _pcm(int(44100 * 0.18), func(t: float, _i: int) -> float:
			return sin(t * 690.0) * 0.5 * (1.0 - t / 0.18))
	elif name == "level":
		wav = _pcm(int(44100 * 0.5), func(t: float, _i: int) -> float:
			var f := 440.0
			if t > 0.1:
				f = 587.0
			if t > 0.2:
				f = 880.0
			return sin(t * 6.283 * f) * 0.22 * (1.0 - t / 0.5))
	elif name == "quest":
		wav = _pcm(int(44100 * 0.45), func(t: float, _i: int) -> float:
			var f := 523.0
			if t > 0.09:
				f = 659.0
			if t > 0.18:
				f = 784.0
			return sin(t * 6.283 * f) * 0.2 * (1.0 - t / 0.45))
	elif name == "boom":
		wav = _pcm(int(44100 * 0.4), func(t: float, i: int) -> float:
			var noise := (float(i % 211) / 211.0 - 0.5) * 0.6
			return (noise + sin(t * 377.0) * 0.4) * (1.0 - t / 0.4))
	elif name == "ui":
		wav = _pcm(int(44100 * 0.05), func(t: float, _i: int) -> float:
			return sin(t * 6030.0) * 0.12 * (1.0 - t / 0.05))
	if wav == null:
		return
	var p := AudioStreamPlayer.new()
	p.stream = wav
	p.volume_db = linear_to_db(0.4 * vol)
	p.set("bus", "SFX")
	add_child(p)
	p.play()
	p.finished.connect(p.queue_free)

func _engine_loop() -> AudioStreamWAV:
	var n := 22050
	var wav := _pcm(n, func(t: float, _i: int) -> float:
		var ph := fmod(t * 110.0, 1.0)
		return (ph * 2.0 - 1.0) * 0.35 + sin(t * 345.0) * 0.2)
	wav.loop_mode = AudioStreamWAV.LOOP_FORWARD
	wav.loop_begin = 0
	wav.loop_end = n - 1
	return wav

func engine_on() -> void:
	engine_player.play()

func engine_off() -> void:
	engine_player.stop()

func engine_speed(sn: float) -> void:
	engine_player.pitch_scale = 0.7 + sn * 1.6
