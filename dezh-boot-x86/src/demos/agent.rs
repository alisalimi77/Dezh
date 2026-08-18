//! The same `.dzp` agent package the RISC-V kernel runs, run here (F3, D003/D016).

use crate::console::{print, print_i64, putb};
use dezh_core::{dzp, ir};

// The x86 implementation of the shared Dezh-core Host: capability checks + the
// actual side effect (serial output). The Dezh-IR engine itself is shared.
struct SerialHost {
    cap: bool,
}
impl ir::Host for SerialHost {
    fn can(&self, cap: u32) -> bool {
        self.cap && cap == ir::CAP_PRINT
    }
    fn print_num(&mut self, v: i64) {
        print("  [ir] => ");
        print_i64(v);
        print("\n");
    }
    fn print_str(&mut self, s: &[u8]) {
        print("  [ir] ");
        for &b in s {
            putb(b);
        }
        putb(b'\n');
    }
    // No block device on x86 yet (M2/M3); Cairn host calls are unavailable.
    fn cairn_put(&mut self, _data: &[u8]) -> bool {
        false
    }
    fn cairn_get(&mut self, _buf: &mut [u8]) -> Option<usize> {
        None
    }
}

/// Install and run a real `.dzp` package: the SAME Dezh-IR bytes the RISC-V
/// kernel runs, wrapped in the SAME architecture-independent `.dzp` format the
/// SDK builds. We pack it, then parse it back exactly as an install flow would
/// (magic + version + CRC + manifest checks) and run the payload. The bytes are
/// pinned byte-identical by dezh-core's `demo_sum_bytes_are_pinned` test, so
/// what installs on one ISA is exactly what runs on the other.
pub(crate) fn run() {
    print("Dezh .dzp agent package (sum 1..=5 with a loop) on x86_64:\n");
    let mut prog_buf = [0u8; 256];
    let prog = ir::demo_sum(&mut prog_buf);
    let manifest = "name = \"agent-sum\"\nversion = \"0.1.0\"\ncaps = [\"print\"]\n";
    let mut pkg = [0u8; 512];
    let n = dzp::pack(dzp::KIND_DEZH_IR, manifest, prog, &mut pkg);
    match dzp::parse(&pkg[..n]) {
        Err(e) => {
            print("  .dzp parse failed: ");
            print(e.msg());
            print("\n");
        }
        Ok(p) => {
            print("  .dzp verified: kind=");
            print(dzp::kind_name(p.kind));
            print(", name=");
            print(dzp::manifest_str(p.manifest, "name").unwrap_or("?"));
            print("\n");
            match ir::verify(p.payload) {
                Err(_) => print("  IR verify failed\n"),
                Ok(()) => {
                    print("  with PRINT capability:\n");
                    let mut h = SerialHost { cap: true };
                    let _ = ir::run(p.payload, &mut h);
                    print("  without PRINT capability:\n");
                    let mut h = SerialHost { cap: false };
                    if ir::run(p.payload, &mut h) == Err(ir::Trap::MissingCapability) {
                        print("  [ir] DENIED: agent holds no PRINT capability\n");
                    }
                }
            }
        }
    }
}
