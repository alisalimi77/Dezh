//! Build script for the kernel:
//!  1. apply our kernel linker script so .text lands where OpenSBI jumps
//!     (0x8020_0000 on the QEMU `virt` board);
//!  2. compile separate user programs to their own riscv ELFs and stage them in
//!     OUT_DIR so the kernel can embed and load them.

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rustc-link-arg=-Tlinker.ld");
    println!("cargo:rerun-if-changed=linker.ld");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    build_user_elf(&manifest, "userprog", "userprog");
    build_user_elf(&manifest, "virtio-blk", "virtio-blk");
}

fn build_user_elf(manifest: &PathBuf, dir: &str, bin: &str) {
    let prog = manifest.join(dir);
    println!("cargo:rerun-if-changed={}", prog.join("src/main.rs").display());
    println!("cargo:rerun-if-changed={}", prog.join("linker.ld").display());
    println!("cargo:rerun-if-changed={}", prog.join("Cargo.toml").display());

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .current_dir(&prog)
        .args(["build", "--release"])
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn cargo for {dir}: {e}"));
    assert!(status.success(), "{dir} build failed");

    let elf = prog.join(format!("target/riscv64gc-unknown-none-elf/release/{bin}"));
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join(format!("{bin}.elf"));
    fs::copy(&elf, &out)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", elf.display(), out.display()));
}
