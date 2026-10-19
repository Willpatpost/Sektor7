//! Minimal GLSL-to-SPIR-V compilation path (T-05, Stage 2): pure-Rust
//! naga compilation at build time — no external shader tools required on
//! the dev pod, CI runners, or the lead's rig. The permanent
//! shader-toolchain decision remains deferred.

use std::env;
use std::fs;
use std::path::PathBuf;

fn compile_shader(stage: &str, file: &str, out_name: &str) {
    let shader_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("shaders");
    let src_path = shader_dir.join(file);
    println!("cargo:rerun-if-changed={}", src_path.display());
    let source = fs::read_to_string(&src_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", src_path.display()));

    let naga_stage = match stage {
        "vertex" => naga::ShaderStage::Vertex,
        "fragment" => naga::ShaderStage::Fragment,
        _ => panic!("unknown shader stage {stage}"),
    };
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(&naga::front::glsl::Options::from(naga_stage), &source)
        .unwrap_or_else(|error| panic!("GLSL parse {file}: {error:?}"));
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|error| panic!("GLSL validation {file}: {error:?}"));
    let words =
        naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
            .unwrap_or_else(|error| panic!("SPIR-V generation {file}: {error:?}"));

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join(out_name);
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for word in &words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    fs::write(&out, bytes).unwrap_or_else(|error| panic!("write {out:?}: {error}"));
}

fn main() {
    compile_shader("vertex", "scene.vert", "scene.vert.spv");
    compile_shader("fragment", "scene.frag", "scene.frag.spv");
    compile_shader("vertex", "overlay.vert", "overlay.vert.spv");
    compile_shader("fragment", "overlay.frag", "overlay.frag.spv");
}
