//! Stamps THOR_BUILD_ID: a hash of the sources this build came from.
//!
//! Every program prints the same release version, so two builds of one release
//! could not be told apart - which is exactly how a tool server older than the
//! hook's checker went unnoticed. The id is a hash of the source files of the
//! crates whose behaviour the programs share (core, model, serve), so two
//! programs built from one tree print one id and a program built before a
//! change prints another. No git is needed, so it also works from a source
//! archive. Line endings are ignored so a checkout does not change it.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().expect("serve sits inside the workspace");
    // FNV-1a: stable across compilers and platforms, which a std hasher is not.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for krate in ["core", "model", "serve"] {
        let src = workspace.join(krate).join("src");
        println!("cargo:rerun-if-changed={}", src.display());
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in files {
            let name = file.strip_prefix(workspace).unwrap_or(&file).to_string_lossy().replace('\\', "/");
            name.bytes().for_each(&mut feed);
            std::fs::read(&file).unwrap_or_default().into_iter().filter(|b| *b != b'\r').for_each(&mut feed);
        }
    }
    println!("cargo:rustc-env=THOR_BUILD_ID={:012x}", hash & 0xffff_ffff_ffff);
}
