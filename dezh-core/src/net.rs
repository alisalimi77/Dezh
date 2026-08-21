//! The internet checksum (RFC 1071), and the one rule about it that is easy to
//! get wrong.
//!
//! This lives here rather than in the `marz` daemon that uses it because of how
//! it failed once. The daemon reads its frames out of a DMA window through
//! volatile loads, so the function it had took `(offset, length)` into a fixed
//! address — which meant there was no input a test could supply, and the only
//! way to exercise it was to boot a machine and send a packet.
//!
//! That is exactly what happened: an odd-length ICMP body had its final byte
//! *dropped* instead of zero-padded, the checksum came out wrong, the host
//! silently discarded the echo request, and the symptom was "no reply ever came
//! back". It was found by reading the code. A three-line test would have caught
//! it the moment it was written.
//!
//! So the reading and the arithmetic are separated. The caller passes an
//! accessor, which keeps its volatile loads where they belong — building a
//! `&[u8]` over a DMA window would be exactly the aliasing the kernel's
//! `Global<T>` exists to prevent — and a test passes an array.

/// Sum `len` bytes as big-endian 16-bit words, fold the carries, and complement.
///
/// `byte(i)` supplies the `i`-th byte, `0 <= i < len`. An odd `len` is defined:
/// the final byte is the **high** half of a word whose low half is zero, which
/// is the same as appending one zero byte. It is not dropped.
///
/// The defining property, and the one worth remembering: inserting this value
/// back into the field it covers makes a fresh checksum over the whole thing
/// come out `0`. That is what a receiver actually checks, and it is the test
/// below that would have caught the bug this module exists because of.
pub fn internet_checksum(len: usize, byte: impl Fn(usize) -> u8) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0usize;
    while i + 1 < len {
        sum += ((byte(i) as u32) << 8) | (byte(i + 1) as u32);
        i += 2;
    }
    if i < len {
        // The odd tail. Zero-padded on the right, never dropped.
        sum += (byte(i) as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// The same over a slice, for callers that already have one.
pub fn internet_checksum_bytes(bytes: &[u8]) -> u16 {
    internet_checksum(bytes.len(), |i| bytes[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real IPv4 header with its checksum field zeroed, from RFC 1071's own
    /// worked example lineage: the answer is fixed and externally checkable.
    const IPV4_HEADER: [u8; 20] = [
        0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8, 0x00,
        0x01, 0xc0, 0xa8, 0x00, 0xc7,
    ];

    #[test]
    fn matches_the_known_ipv4_header_value() {
        assert_eq!(internet_checksum_bytes(&IPV4_HEADER), 0xb861);
    }

    /// The receiver's test: put the checksum back in the field it covers and a
    /// fresh sum over the whole header is zero.
    #[test]
    fn inserting_the_checksum_makes_a_fresh_sum_zero() {
        let mut header = IPV4_HEADER;
        let csum = internet_checksum_bytes(&header);
        header[10] = (csum >> 8) as u8;
        header[11] = csum as u8;
        assert_eq!(internet_checksum_bytes(&header), 0);
    }

    /// The bug this module exists because of. An odd-length body must be summed
    /// as though one zero byte followed it; dropping the final byte produces a
    /// different value, and the difference is invisible until a real host
    /// silently discards the packet.
    #[test]
    fn an_odd_tail_byte_is_padded_not_dropped() {
        let odd = [0x01u8, 0x02, 0x03];
        let padded = [0x01u8, 0x02, 0x03, 0x00];
        let truncated = [0x01u8, 0x02];

        assert_eq!(internet_checksum_bytes(&odd), internet_checksum_bytes(&padded));
        assert_ne!(internet_checksum_bytes(&odd), internet_checksum_bytes(&truncated));
    }

    /// The tail is the *high* half of the word, not the low half. Getting this
    /// backwards also passes the "not dropped" test above.
    #[test]
    fn the_odd_tail_is_the_high_half_of_its_word() {
        assert_eq!(
            internet_checksum_bytes(&[0xab]),
            internet_checksum_bytes(&[0xab, 0x00])
        );
        assert_ne!(
            internet_checksum_bytes(&[0xab]),
            internet_checksum_bytes(&[0x00, 0xab])
        );
    }

    /// Carries fold back in rather than being discarded: two words that sum past
    /// 16 bits must wrap into the low half.
    #[test]
    fn carries_fold_back_into_the_low_half() {
        // 0xffff + 0xffff = 0x1fffe -> fold -> 0xffff -> complement -> 0x0000
        assert_eq!(internet_checksum_bytes(&[0xff, 0xff, 0xff, 0xff]), 0x0000);
        // A single 0xffff word: complement is zero, and folding must not loop.
        assert_eq!(internet_checksum_bytes(&[0xff, 0xff]), 0x0000);
    }

    #[test]
    fn all_zero_input_is_all_ones() {
        assert_eq!(internet_checksum_bytes(&[0u8; 20]), 0xffff);
        assert_eq!(internet_checksum_bytes(&[]), 0xffff);
    }

    /// The accessor form and the slice form are the same function.
    #[test]
    fn the_accessor_and_slice_forms_agree() {
        let data = [0x45u8, 0x00, 0x00, 0x73, 0x11, 0x07];
        assert_eq!(
            internet_checksum(data.len(), |i| data[i]),
            internet_checksum_bytes(&data)
        );
    }

    /// A checksum is over bytes, not over where they came from: the accessor may
    /// read from anywhere, and an offset view must give the same answer as the
    /// slice it views.
    #[test]
    fn an_offset_accessor_sees_only_its_own_window() {
        let backing = [0xde, 0xad, 0x45, 0x00, 0x00, 0x73, 0xbe, 0xef];
        let window = &backing[2..6];
        assert_eq!(
            internet_checksum(window.len(), |i| backing[2 + i]),
            internet_checksum_bytes(window)
        );
    }
}
