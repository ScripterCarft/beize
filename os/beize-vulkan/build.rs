//! The renderer's shaders: GLSL compiled to SPIR-V by glslang, Khronos'
//! reference compiler, and checked by spirv-val (both in the buildroot,
//! ADR 0027 §2). Without debug information, so no path of the build
//! machine reaches the program.

use std::path::{Path, PathBuf};
use std::process::Command;

const SHADERS: [&str; 3] = ["quad.vert", "quad.frag", "backdrop.frag"];
const INCLUDES: [&str; 1] = ["quad.glsl"];
const TARGET: &str = "vulkan1.3";

fn main() -> Result<(), String> {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    for name in INCLUDES {
        println!("cargo::rerun-if-changed=shaders/{name}");
    }
    for name in SHADERS {
        println!("cargo::rerun-if-changed=shaders/{name}");
        let spv = out.join(format!("{name}.spv"));
        run(Command::new("glslangValidator")
            .args(["--quiet", "--target-env", TARGET, "-o"])
            .arg(&spv)
            .arg(Path::new("shaders").join(name)))?;
        // The quads' buffer is scalar (the device enables scalarBlockLayout).
        run(Command::new("spirv-val")
            .args(["--target-env", TARGET, "--scalar-block-layout"])
            .arg(&spv))?;
    }
    Ok(())
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("{cmd:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd:?}: {status}"))
    }
}
