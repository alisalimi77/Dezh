//! `.dzp` — the Dezh package format (W1).
//!
//! One package = one installable app: a fixed header, a small TOML manifest
//! (name, version, requested capabilities), and the payload the kernel runs
//! (Dezh-IR bytecode today; a riscv64 ELF for native apps). The format is
//! architecture-independent on purpose: F3 requires the *same bytes* to install
//! and run on the RISC-V and x86_64 kernels, so the parser lives here in
//! `dezh-core` and is `alloc`-free (borrows from the input buffer).
//!
//! Layout (little-endian):
//!
//! ```text
//! 0   magic  "DZP1"
//! 4   u16    format version (1)
//! 6   u16    payload kind (1 = dezh-ir, 2 = elf-riscv64)
//! 8   u32    manifest length
//! 12  u32    payload length
//! 16  u32    CRC-32 (IEEE) of manifest || payload
//! 20  manifest bytes (UTF-8, app.toml subset)
//! ..  payload bytes
//! ```

pub const MAGIC: &[u8; 4] = b"DZP1";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 20;

pub const KIND_DEZH_IR: u16 = 1;
pub const KIND_ELF_RISCV64: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DzpError {
    TooShort,
    BadMagic,
    BadVersion,
    BadKind,
    Truncated,
    BadCrc,
    ManifestNotUtf8,
}

impl DzpError {
    pub fn msg(self) -> &'static str {
        match self {
            DzpError::TooShort => "shorter than the fixed header",
            DzpError::BadMagic => "bad magic (not a .dzp package)",
            DzpError::BadVersion => "unsupported format version",
            DzpError::BadKind => "unknown payload kind",
            DzpError::Truncated => "declared lengths exceed the package",
            DzpError::BadCrc => "checksum mismatch (corrupt upload?)",
            DzpError::ManifestNotUtf8 => "manifest is not valid UTF-8",
        }
    }
}

/// A parsed package borrowing from the raw bytes.
#[derive(Debug)]
pub struct Package<'a> {
    pub kind: u16,
    pub manifest: &'a str,
    pub payload: &'a [u8],
}

pub fn kind_name(kind: u16) -> &'static str {
    match kind {
        KIND_DEZH_IR => "dezh-ir",
        KIND_ELF_RISCV64 => "elf-riscv64",
        _ => "unknown",
    }
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// CRC-32 (IEEE 802.3, reflected), bitwise — no table, fine for install-time use.
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for part in parts {
        for &byte in *part {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }
    !crc
}

/// Parse and integrity-check a `.dzp` byte buffer.
pub fn parse(bytes: &[u8]) -> Result<Package<'_>, DzpError> {
    if bytes.len() < HEADER_LEN {
        return Err(DzpError::TooShort);
    }
    if &bytes[0..4] != MAGIC {
        return Err(DzpError::BadMagic);
    }
    if u16_at(bytes, 4) != VERSION {
        return Err(DzpError::BadVersion);
    }
    let kind = u16_at(bytes, 6);
    if kind != KIND_DEZH_IR && kind != KIND_ELF_RISCV64 {
        return Err(DzpError::BadKind);
    }
    let mlen = u32_at(bytes, 8) as usize;
    let plen = u32_at(bytes, 12) as usize;
    let total = HEADER_LEN
        .checked_add(mlen)
        .and_then(|t| t.checked_add(plen))
        .ok_or(DzpError::Truncated)?;
    if bytes.len() != total {
        return Err(DzpError::Truncated);
    }
    let manifest_bytes = &bytes[HEADER_LEN..HEADER_LEN + mlen];
    let payload = &bytes[HEADER_LEN + mlen..];
    if u32_at(bytes, 16) != crc32(&[manifest_bytes, payload]) {
        return Err(DzpError::BadCrc);
    }
    let manifest = core::str::from_utf8(manifest_bytes).map_err(|_| DzpError::ManifestNotUtf8)?;
    Ok(Package {
        kind,
        manifest,
        payload,
    })
}

// --- Manifest (app.toml subset) ------------------------------------------------
// The kernel only needs `name`, `version`, and `caps`; everything else in the
// manifest is for host-side tooling. Grammar accepted here: one `key = value`
// per line, `#` comments, string values in double quotes, and a single-line
// string array for `caps`.

/// Look up a quoted string value, e.g. `name = "hello"`.
pub fn manifest_str<'a>(manifest: &'a str, key: &str) -> Option<&'a str> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        let inner = v.strip_prefix('"')?;
        return inner.split_once('"').map(|(s, _)| s);
    }
    None
}

/// Iterate the quoted items of a single-line string array, e.g.
/// `caps = ["print", "ipc"]`.
pub fn manifest_list<'a>(manifest: &'a str, key: &str) -> ManifestList<'a> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            return ManifestList { rest: inner };
        }
    }
    ManifestList { rest: "" }
}

pub struct ManifestList<'a> {
    rest: &'a str,
}

impl<'a> Iterator for ManifestList<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        let open = self.rest.find('"')?;
        let after = &self.rest[open + 1..];
        let close = after.find('"')?;
        self.rest = &after[close + 1..];
        Some(&after[..close])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(kind: u16, manifest: &str, payload: &[u8], out: &mut [u8]) -> usize {
        let m = manifest.as_bytes();
        out[0..4].copy_from_slice(MAGIC);
        out[4..6].copy_from_slice(&VERSION.to_le_bytes());
        out[6..8].copy_from_slice(&kind.to_le_bytes());
        out[8..12].copy_from_slice(&(m.len() as u32).to_le_bytes());
        out[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        out[16..20].copy_from_slice(&crc32(&[m, payload]).to_le_bytes());
        out[20..20 + m.len()].copy_from_slice(m);
        out[20 + m.len()..20 + m.len() + payload.len()].copy_from_slice(payload);
        20 + m.len() + payload.len()
    }

    const MANIFEST: &str = "name = \"hello\"\nversion = \"0.1.0\"\ncaps = [\"print\", \"ipc\"]\n";

    #[test]
    fn roundtrip() {
        let mut buf = [0u8; 256];
        let n = build(KIND_DEZH_IR, MANIFEST, &[0x00], &mut buf);
        let pkg = parse(&buf[..n]).unwrap();
        assert_eq!(pkg.kind, KIND_DEZH_IR);
        assert_eq!(pkg.payload, &[0x00]);
        assert_eq!(manifest_str(pkg.manifest, "name"), Some("hello"));
        assert_eq!(manifest_str(pkg.manifest, "version"), Some("0.1.0"));
        let caps: [&str; 2] = {
            let mut it = manifest_list(pkg.manifest, "caps");
            [it.next().unwrap(), it.next().unwrap()]
        };
        assert_eq!(caps, ["print", "ipc"]);
    }

    #[test]
    fn rejects_corruption() {
        let mut buf = [0u8; 256];
        let n = build(KIND_DEZH_IR, MANIFEST, &[0x00], &mut buf);
        buf[HEADER_LEN + 2] ^= 0xFF;
        assert_eq!(parse(&buf[..n]).unwrap_err(), DzpError::BadCrc);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = [0u8; 256];
        let n = build(KIND_DEZH_IR, MANIFEST, &[0x00], &mut buf);
        buf[0] = b'X';
        assert_eq!(parse(&buf[..n]).unwrap_err(), DzpError::BadMagic);
    }

    #[test]
    fn rejects_truncation() {
        let mut buf = [0u8; 256];
        let n = build(KIND_DEZH_IR, MANIFEST, &[0x00, 0x01], &mut buf);
        assert_eq!(parse(&buf[..n - 1]).unwrap_err(), DzpError::Truncated);
    }

    #[test]
    fn empty_caps_iterates_nothing() {
        assert_eq!(manifest_list("caps = []", "caps").count(), 0);
        assert_eq!(manifest_list("name = \"x\"", "caps").count(), 0);
    }
}
