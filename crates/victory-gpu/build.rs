//! Stable shader cache identity across changes outside the translator.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, paths: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(&entry.path(), paths)?;
        } else if kind.is_file() {
            paths.push(entry.path());
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let shader = Path::new("../victory-shader/src");
    let mut files = vec![
        PathBuf::from("../victory-shader/Cargo.toml"),
        PathBuf::from("../../Cargo.toml"),
        PathBuf::from("../../Cargo.lock"),
        PathBuf::from("src/shader_cache.rs"),
    ];
    println!("cargo:rerun-if-changed={}", shader.display());
    collect(shader, &mut files)?;
    files.sort();
    let mut hash = blake3::Hasher::new();
    for name in files {
        println!("cargo:rerun-if-changed={}", name.display());
        let path = name.to_str().ok_or("non-Unicode translator path")?.replace('\\', "/");
        let bytes = std::fs::read(name)?;
        for field in [path.as_bytes(), &bytes] {
            hash.update(&(field.len() as u64).to_le_bytes());
            hash.update(field);
        }
    }
    // Do not share across targets or compiler releases (codegen can differ).
    hash.update(std::env::var("TARGET")?.as_bytes());
    let version = std::process::Command::new(std::env::var_os("RUSTC").ok_or("no RUSTC")?)
        .arg("--version")
        .output()?;
    if !version.status.success() {
        return Err("rustc --version failed".into());
    }
    hash.update(&version.stdout);
    println!("cargo:rustc-env=VICTORY_TRANSLATOR_HASH={}", hash.finalize().to_hex());
    Ok(())
}
