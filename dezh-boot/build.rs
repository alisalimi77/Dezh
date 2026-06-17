//! Build script for the kernel:
//!  1. apply our kernel linker script so .text lands where OpenSBI jumps
//!     (0x8020_0000 on the QEMU `virt` board);
//!  2. compile the separate user program to its own riscv ELF and stage it in
//!     OUT_DIR so the kernel can embed and load it.

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rustc-link-arg=-Tlinker.ld");
    println!("cargo:rerun-if-changed=linker.ld");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let userprog = manifest.join("userprog");
    println!(
        "cargo:rerun-if-changed={}",
        userprog.join("src/main.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        userprog.join("linker.ld").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        userprog.join("Cargo.toml").display()
    );

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .current_dir(&userprog)
        .args(["build", "--release"])
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .status()
        .expect("failed to spawn cargo for userprog");
    assert!(status.success(), "userprog build failed");

    let elf = userprog.join("target/riscv64gc-unknown-none-elf/release/userprog");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("userprog.elf");
    fs::copy(&elf, &out)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", elf.display(), out.display()));
}
