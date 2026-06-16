//! Compiles the example guests to `wasm32-unknown-unknown` and stages the
//! resulting `.wasm` into `OUT_DIR`, where `src/lib.rs` embeds them with
//! `include_bytes!`.
//!
//! The guests live in their own workspace (`../guests`) with their own target
//! directory, so this recursive `cargo` invocation does not contend for the
//! host build's lock. We also scrub host build-env vars that would otherwise
//! leak into the guest compile.

use std::{env, fs, path::PathBuf, process::Command};

const GUESTS: [&str; 3] = ["g_granted", "g_denied", "g_attenuate"];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest.parent().unwrap().to_path_buf();
    let guests_dir = workspace_root.join("guests");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Rebuild whenever any guest source or manifest changes.
    println!(
        "cargo:rerun-if-changed={}",
        guests_dir.join("Cargo.toml").display()
    );
    for g in GUESTS {
        println!(
            "cargo:rerun-if-changed={}",
            guests_dir.join(g).join("src").join("lib.rs").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            guests_dir.join(g).join("Cargo.toml").display()
        );
    }

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(&guests_dir)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"]);
    for g in GUESTS {
        cmd.args(["-p", g]);
    }
    // Don't let the host build's flags/target/wrappers bleed into the guests.
    cmd.env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER");

    let status = cmd.status().expect("failed to spawn cargo for guest build");
    assert!(status.success(), "guest wasm build failed");

    let wasm_dir = guests_dir.join("target/wasm32-unknown-unknown/release");
    for g in GUESTS {
        let src = wasm_dir.join(format!("{g}.wasm"));
        let dst = out_dir.join(format!("{g}.wasm"));
        fs::copy(&src, &dst)
            .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dst.display()));
    }
}
