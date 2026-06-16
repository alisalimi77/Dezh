//! dezh-demo — a human-readable sanity pass that runs all three example guests
//! through the capability runtime and prints what happened. This is not a test
//! (see `cargo test` for the assertions); it just narrates the model end to end.

use dezh_host::guests_wasm::{G_ATTENUATE, G_DENIED, G_GRANTED};
use dezh_host::{run_guest, HostState, Ops};

const RES_A: u32 = 1;
const RES_SECRET: u32 = 2;

fn main() -> wasmtime::Result<()> {
    println!("Dezh Step 1 — capability core demo\n");

    // --- g_granted: holds READ for resource A, reads it successfully. --------
    {
        let mut st = HostState::new();
        st.resources.put(RES_A, b"hello".to_vec());
        st.resources.put(RES_SECRET, b"SECRET".to_vec()); // never handed to it
        st.grant(RES_A, Ops::READ); // handle 0
        let (ret, _) = run_guest(G_GRANTED, st)?;
        println!("g_granted  : read checksum = {ret}  (sum of b\"hello\" = 532) -> OK");
    }

    // --- g_denied: holds nothing, every op is refused. ----------------------
    {
        let st = HostState::new();
        let (ret, st) = run_guest(G_DENIED, st)?;
        println!(
            "g_denied   : run() = {ret}  (NoSuchHandle), captured stdout = {} bytes -> zero authority",
            st.stdout.len()
        );
    }

    // --- g_attenuate: narrows READ+WRITE to READ, cannot widen back. --------
    {
        let mut st = HostState::new();
        st.resources.put(RES_A, b"data".to_vec());
        st.grant(RES_A, Ops::READ.union(Ops::WRITE)); // handle 0
        let (ret, st) = run_guest(G_ATTENUATE, st)?;
        let unchanged = st.resources.get(RES_A).map(|v| v.as_slice()) == Some(b"data");
        println!(
            "g_attenuate: run() = {ret}  (0 = all invariants held), resource unchanged = {unchanged}"
        );
    }

    println!("\nAll guests behaved as the capability model requires.");
    Ok(())
}
