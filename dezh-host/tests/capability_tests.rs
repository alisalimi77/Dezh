//! End-to-end capability tests: real wasm guests driven through the host
//! runtime. These cover the four Definition-of-Done properties at the wasm
//! boundary (the unit tests in `src/lib.rs` cover the pure core).

use dezh_host::guests_wasm::{G_ATTENUATE, G_DENIED, G_GRANTED};
use dezh_host::{run_guest, CapError, HostState, Ops};

const RES_A: u32 = 1;
const RES_SECRET: u32 = 2;

/// DoD #1 — a guest with no capability cannot read, write, or print anything.
#[test]
fn no_capability_denies_everything() {
    let mut st = HostState::new();
    // Seed a resource the guest has NO capability for.
    st.resources.put(RES_A, b"hello".to_vec());

    let (ret, st) = run_guest(G_DENIED, st).expect("guest runs");

    // Guest returns the read error when read+write+print were all denied.
    assert_eq!(ret, CapError::NoSuchHandle.code() as i64);
    // Nothing was printed and the resource was not mutated.
    assert!(st.stdout.is_empty(), "denied guest must not print");
    assert_eq!(st.resources.get(RES_A).unwrap().as_slice(), b"hello");
}

/// DoD #2 — a guest can access ONLY the resource it was granted; another
/// resource it was never handed a capability for is unreachable.
#[test]
fn granted_guest_reads_only_its_resource() {
    let mut st = HostState::new();
    st.resources.put(RES_A, b"hello".to_vec()); // checksum 532
    st.resources.put(RES_SECRET, b"SECRET".to_vec()); // never granted
    st.grant(RES_A, Ops::READ); // handle 0 -> resource A

    let (ret, st) = run_guest(G_GRANTED, st).expect("guest runs");

    // 532 = sum of bytes of "hello" -> it read resource A's real content.
    assert_eq!(ret, 532, "guest must read the granted resource's content");
    // The guest holds exactly one handle; there is no handle naming RES_SECRET.
    assert_eq!(st.caps.len(), 1);
    assert_eq!(st.caps.check(1, Ops::READ), Err(CapError::NoSuchHandle));
}

/// DoD #3 — a guest cannot forge or guess a valid handle. Out-of-range and
/// made-up handles are rejected at the enforcement point.
#[test]
fn forged_handles_are_rejected() {
    let mut st = HostState::new();
    st.resources.put(RES_A, b"x".to_vec());
    st.grant(RES_A, Ops::READ); // only handle 0 exists

    // Handle 0 is real; everything else is a forgery.
    assert!(st.caps.check(0, Ops::READ).is_ok());
    assert_eq!(st.caps.check(1, Ops::READ), Err(CapError::NoSuchHandle));
    assert_eq!(st.caps.check(42, Ops::READ), Err(CapError::NoSuchHandle));
    assert_eq!(
        st.caps.check(u32::MAX, Ops::READ),
        Err(CapError::NoSuchHandle)
    );
}

/// DoD #4 — attenuation yields a strictly narrower capability, and there is no
/// path to widen one. The guest itself performs the narrow/write-denied/
/// read-ok/widen-rejected/no-op-rejected sequence and returns 0 iff all held.
#[test]
fn attenuation_narrows_and_never_widens() {
    let mut st = HostState::new();
    st.resources.put(RES_A, b"data".to_vec());
    st.grant(RES_A, Ops::READ.union(Ops::WRITE)); // handle 0

    let (ret, st) = run_guest(G_ATTENUATE, st).expect("guest runs");

    assert_eq!(
        ret, 0,
        "every attenuation invariant must hold (bitmask of failures)"
    );
    // The write through the attenuated READ-only cap was denied, so RES_A is
    // unchanged; and a child handle was installed (handle 1).
    assert_eq!(st.resources.get(RES_A).unwrap().as_slice(), b"data");
    assert_eq!(st.caps.len(), 2);
}
