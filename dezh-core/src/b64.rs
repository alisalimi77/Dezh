//! Minimal base64 decoder (RFC 4648, standard alphabet) for the UART package
//! upload path. `alloc`-free; shared by every ISA kernel that accepts `.dzp`
//! uploads over a serial console.

/// Decode one base64 chunk into `out`, returning the byte count.
/// Accepts `=` padding; rejects any other non-alphabet character.
pub fn decode(input: &[u8], out: &mut [u8]) -> Option<usize> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let mut n = 0usize;
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut padded = false;
    for &c in input {
        if c == b'=' {
            padded = true;
            continue;
        }
        if padded {
            return None; // data after padding
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if n >= out.len() {
                return None;
            }
            out[n] = (acc >> bits) as u8;
            n += 1;
        }
    }
    // Leftover bits must be zero-padding only (4 or 2 bits).
    if bits > 0 && acc & ((1 << bits) - 1) != 0 {
        return None;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Option<alloc_free_vec::Buf> {
        let mut out = alloc_free_vec::Buf {
            data: [0; 64],
            len: 0,
        };
        let n = decode(s.as_bytes(), &mut out.data)?;
        out.len = n;
        Some(out)
    }

    mod alloc_free_vec {
        pub struct Buf {
            pub data: [u8; 64],
            pub len: usize,
        }
        impl Buf {
            pub fn as_slice(&self) -> &[u8] {
                &self.data[..self.len]
            }
        }
    }

    #[test]
    fn decodes_known_vectors() {
        assert_eq!(dec("aGVsbG8=").unwrap().as_slice(), b"hello");
        assert_eq!(dec("aGVsbG8h").unwrap().as_slice(), b"hello!");
        assert_eq!(dec("aA==").unwrap().as_slice(), b"h");
        assert_eq!(dec("").unwrap().as_slice(), b"");
    }

    #[test]
    fn rejects_garbage() {
        assert!(dec("a$b=").is_none());
        assert!(dec("aA==aA==").is_none()); // data after padding
    }
}
