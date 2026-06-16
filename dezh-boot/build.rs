//! Apply our kernel linker script so .text lands at the address OpenSBI jumps
//! to on the QEMU `virt` board (0x8020_0000).
fn main() {
    println!("cargo:rustc-link-arg=-Tlinker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
}
