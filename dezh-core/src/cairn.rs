//! The Cairn v1 commit record: the on-disk shape of an effect.
//!
//! Every commit is one sector — a fixed header and an inline value — and the
//! header is what the ledger is made of: who acted, under which intent, with
//! which derived capability, how reversible the effect is, and which commit it
//! is a child of. `sfar-plan`, `sand-log` and the rollback path all read it, and
//! the rollback path in particular *decides what it is allowed to undo* from
//! these bytes.
//!
//! It lives here for the reason the checksum and the ticket queue do. In the
//! `virtio-blk` daemon the encode and decode were open-coded against a fixed
//! data pointer — `d_write_u32(36, intent)` on one side, `d_read_u32(36)` on the
//! other, thirty-three offsets apart, with the `u16` generation split into two
//! bytes by hand. No test could supply a record, so the only check the format
//! ever had was that a whole QEMU run agreed with itself. Two sides of a format
//! that are wrong the same way agree perfectly.
//!
//! The accessor pattern is the same too: `encode` takes a writer and `decode` a
//! reader, so the daemon's volatile access to the DMA page stays where it
//! belongs and a test passes an array.

/// Bytes before the inline value. A record's header occupies `0..VALUE_OFF`.
pub const VALUE_OFF: usize = 64;

/// `DZC1` — unchanged since v1. Sand enrichment is additive within the header's
/// previously-spare span, never a second log and never a new magic.
pub const MAGIC: [u8; 4] = *b"DZC1";

/// Commit slots in the log. There is no GC: the 256th commit is refused.
pub const COMMIT_SLOTS: u32 = 255;

/// "No parent" / "no head" — a namespace that has never been committed to.
pub const NONE: u32 = 0xffff_ffff;

/// Reversibility, as recorded on the effect itself.
pub mod rev {
    /// Undo by moving the ref (an ordinary Cairn commit).
    pub const REVERSIBLE: u8 = 0;
    /// Undo needs a compensating effect, which the connector must have
    /// registered at commit time.
    pub const COMPENSATABLE: u8 = 1;
    /// Cannot be undone — it happened in the outside world.
    pub const IRREVERSIBLE: u8 = 2;
    /// The connector did not declare. **Never** treated as reversible.
    pub const UNKNOWN: u8 = 3;
}

/// Effect status.
pub mod status {
    pub const COMMITTED: u8 = 0;
    /// A compensating action recorded during a rollback. It is itself a
    /// first-class effect: the honest undo of a compensatable effect is to
    /// perform and record an inverse, never to erase the record.
    pub const COMPENSATION: u8 = 2;
}

/// The header of one commit record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitHeader {
    /// This record's own slot in the log.
    pub slot: u32,
    /// The commit this one is a child of, or [`NONE`].
    pub parent: u32,
    pub ns: u32,
    /// FNV-1a over the value bytes.
    pub hash: u64,
    /// The kernel task id that acted.
    pub actor: u32,
    /// Bit 0: reversible.
    pub flags: u32,
    /// Value length, in bytes, inline after [`VALUE_OFF`].
    pub len: u32,
    /// The Ahd this effect was authorized under. `0` means direct — no intent.
    pub intent: u32,
    /// The capability set derived under that intent.
    pub derived: u32,
    pub rev_class: u8,
    pub status: u8,
    /// This effect's position on its namespace chain, from 1.
    pub generation: u16,
}

// Field offsets. Named because the daemon and this module have to agree about
// them, and a number repeated in two files is how they stop agreeing.
const OFF_SLOT: usize = 4;
const OFF_PARENT: usize = 8;
const OFF_NS: usize = 12;
const OFF_HASH: usize = 16;
const OFF_ACTOR: usize = 24;
const OFF_FLAGS: usize = 28;
const OFF_LEN: usize = 32;
const OFF_INTENT: usize = 36;
const OFF_DERIVED: usize = 40;
const OFF_REVCLASS: usize = 44;
const OFF_STATUS: usize = 45;
const OFF_GEN: usize = 46;

fn put_u32(write: &mut impl FnMut(usize, u8), off: usize, v: u32) {
    let mut i = 0;
    while i < 4 {
        write(off + i, (v >> (8 * i)) as u8);
        i += 1;
    }
}

fn get_u32(read: &impl Fn(usize) -> u8, off: usize) -> u32 {
    let mut v = 0u32;
    let mut i = 0;
    while i < 4 {
        v |= (read(off + i) as u32) << (8 * i);
        i += 1;
    }
    v
}

impl CommitHeader {
    /// Write the header through `write(offset, byte)`.
    ///
    /// Only the header is written. The caller supplies the value bytes after
    /// [`VALUE_OFF`], and is responsible for the rest of the sector being zero —
    /// which is what makes the Sand span additive on a store formatted before it
    /// existed.
    pub fn encode(&self, mut write: impl FnMut(usize, u8)) {
        let mut i = 0;
        while i < MAGIC.len() {
            write(i, MAGIC[i]);
            i += 1;
        }
        put_u32(&mut write, OFF_SLOT, self.slot);
        put_u32(&mut write, OFF_PARENT, self.parent);
        put_u32(&mut write, OFF_NS, self.ns);
        put_u32(&mut write, OFF_HASH, self.hash as u32);
        put_u32(&mut write, OFF_HASH + 4, (self.hash >> 32) as u32);
        put_u32(&mut write, OFF_ACTOR, self.actor);
        put_u32(&mut write, OFF_FLAGS, self.flags);
        put_u32(&mut write, OFF_LEN, self.len);
        put_u32(&mut write, OFF_INTENT, self.intent);
        put_u32(&mut write, OFF_DERIVED, self.derived);
        write(OFF_REVCLASS, self.rev_class);
        write(OFF_STATUS, self.status);
        write(OFF_GEN, self.generation as u8);
        write(OFF_GEN + 1, (self.generation >> 8) as u8);
    }

    /// Read a header back, or `None` when the magic does not match.
    pub fn decode(read: impl Fn(usize) -> u8) -> Option<Self> {
        let mut i = 0;
        while i < MAGIC.len() {
            if read(i) != MAGIC[i] {
                return None;
            }
            i += 1;
        }
        Some(Self {
            slot: get_u32(&read, OFF_SLOT),
            parent: get_u32(&read, OFF_PARENT),
            ns: get_u32(&read, OFF_NS),
            hash: (get_u32(&read, OFF_HASH) as u64)
                | ((get_u32(&read, OFF_HASH + 4) as u64) << 32),
            actor: get_u32(&read, OFF_ACTOR),
            flags: get_u32(&read, OFF_FLAGS),
            len: get_u32(&read, OFF_LEN),
            intent: get_u32(&read, OFF_INTENT),
            derived: get_u32(&read, OFF_DERIVED),
            rev_class: read(OFF_REVCLASS),
            status: read(OFF_STATUS),
            generation: (read(OFF_GEN) as u16) | ((read(OFF_GEN + 1) as u16) << 8),
        })
    }
}

/// May a commit be written into slot `next`?
///
/// There is no garbage collection, so the log fills once and stays full. The
/// boundary is off by one in the direction that matters: slots `0..=254` are
/// usable, `255` is not, and a commit that lands on `255` must be refused rather
/// than wrapping onto slot 0 and overwriting the first effect ever recorded.
pub const fn slot_available(next: u32) -> bool {
    next < COMMIT_SLOTS
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::vec;
    use std::vec::Vec;

    fn sector() -> Vec<u8> {
        vec![0u8; 512]
    }

    fn sample() -> CommitHeader {
        CommitHeader {
            slot: 19,
            parent: 18,
            ns: 3,
            hash: 0x7378_c6ec_f8dd_4b1f,
            actor: 1,
            flags: 1,
            len: 55,
            intent: 8,
            derived: 0x0000_002a,
            rev_class: rev::IRREVERSIBLE,
            status: status::COMMITTED,
            generation: 300,
        }
    }

    #[test]
    fn a_header_survives_a_round_trip() {
        let mut buf = sector();
        sample().encode(|off, b| buf[off] = b);
        assert_eq!(CommitHeader::decode(|off| buf[off]), Some(sample()));
    }

    #[test]
    fn a_wrong_magic_decodes_to_nothing() {
        let mut buf = sector();
        sample().encode(|off, b| buf[off] = b);
        buf[2] = b'X';
        assert!(CommitHeader::decode(|off| buf[off]).is_none());
        // And an unformatted sector is not mistaken for a record.
        let empty = sector();
        assert!(CommitHeader::decode(|off| empty[off]).is_none());
    }

    /// The compatibility claim the format makes in its own comment: Sand's span
    /// is additive, so a store formatted before Sand existed reads back as a
    /// direct commit with no intent rather than as garbage.
    #[test]
    fn a_pre_sand_record_reads_as_a_direct_commit() {
        let mut buf = sector();
        // Only the v1 fields, exactly as a pre-Sand daemon would have written
        // them: magic, slot, parent, ns, hash, actor, flags, len. Everything
        // from offset 36 on stays zero.
        let pre_sand = CommitHeader {
            intent: 0,
            derived: 0,
            rev_class: 0,
            status: 0,
            generation: 0,
            ..sample()
        };
        pre_sand.encode(|off, b| buf[off] = b);
        for byte in buf.iter().take(VALUE_OFF).skip(36) {
            assert_eq!(*byte, 0, "the Sand span must be zero in this fixture");
        }

        let got = CommitHeader::decode(|off| buf[off]).expect("still a valid record");
        assert_eq!(got.intent, 0, "no Ahd means direct");
        assert_eq!(got.rev_class, rev::REVERSIBLE);
        assert_eq!(got.status, status::COMMITTED);
        assert_eq!(got.slot, 19, "the v1 fields are untouched");
    }

    /// The generation is a `u16` split into two bytes by hand, which is where an
    /// endianness slip hides: a value under 256 round-trips either way.
    #[test]
    fn the_generation_is_little_endian_across_both_its_bytes() {
        let mut buf = sector();
        let h = CommitHeader {
            generation: 0x0102,
            ..sample()
        };
        h.encode(|off, b| buf[off] = b);
        assert_eq!(buf[OFF_GEN], 0x02, "low byte first");
        assert_eq!(buf[OFF_GEN + 1], 0x01);
        assert_eq!(CommitHeader::decode(|off| buf[off]).unwrap().generation, 0x0102);
    }

    /// Every field lands in its own bytes. A header written over a sector that
    /// already held one must not leave any of the old values behind, and no
    /// field may overlap its neighbour.
    #[test]
    fn no_field_overlaps_another() {
        let mut buf = sector();
        let a = sample();
        a.encode(|off, b| buf[off] = b);

        let b = CommitHeader {
            slot: 0xaaaa_aaaa,
            parent: 0xbbbb_bbbb,
            ns: 0xcccc_cccc,
            hash: 0xdddd_dddd_eeee_eeee,
            actor: 0x1111_1111,
            flags: 0x2222_2222,
            len: 0x3333_3333,
            intent: 0x4444_4444,
            derived: 0x5555_5555,
            rev_class: 0x66,
            status: 0x77,
            generation: 0x8899,
        };
        b.encode(|off, x| buf[off] = x);
        assert_eq!(CommitHeader::decode(|off| buf[off]), Some(b));
    }

    #[test]
    fn the_value_never_reaches_the_header() {
        let mut buf = sector();
        // A value written first must survive the header being written over it.
        for (i, slot) in buf.iter_mut().enumerate().skip(VALUE_OFF) {
            *slot = (i % 251) as u8;
        }
        sample().encode(|off, b| {
            assert!(off < VALUE_OFF, "encode must not write past the header");
            buf[off] = b;
        });
        for (i, byte) in buf.iter().enumerate().skip(VALUE_OFF) {
            assert_eq!(*byte, (i % 251) as u8, "the value was disturbed");
        }
    }

    /// The 255-slot boundary, with no GC behind it. Slot 255 must be refused,
    /// not wrapped onto slot 0 — which would overwrite the first effect ever
    /// recorded, and the rollback path reads that record to decide what it may
    /// undo.
    #[test]
    fn the_log_refuses_the_slot_past_its_last() {
        assert!(slot_available(0));
        assert!(slot_available(COMMIT_SLOTS - 1), "slot 254 is the last usable one");
        assert!(!slot_available(COMMIT_SLOTS), "slot 255 is one past the end");
        assert!(!slot_available(COMMIT_SLOTS + 1));
        assert!(!slot_available(u32::MAX));
    }

    /// `NONE` is not a slot. A namespace with no head must never be walked as
    /// though it pointed at one.
    ///
    /// Checked at compile time rather than in the test body: it is a fact about
    /// the format, so a change that broke it should fail the build.
    ///
    /// Stated as an inequality rather than `NONE >= COMMIT_SLOTS`, which reads
    /// like the stronger claim but is vacuous — `NONE` is `u32::MAX`, so that
    /// comparison holds for every possible slot count including a wrong one.
    const _: () = assert!(COMMIT_SLOTS != NONE);

    #[test]
    fn the_no_parent_sentinel_is_not_a_usable_slot() {
        assert!(!slot_available(NONE));
    }

    /// Walking a parent chain has to terminate even when the chain is a cycle,
    /// which a torn write could produce. The bound is the log's own size: no
    /// honest chain can be longer than the number of slots.
    #[test]
    fn a_cyclic_parent_chain_terminates_under_the_slot_bound() {
        // 3 -> 2 -> 1 -> 3 -> ...
        let parent_of = |slot: u32| match slot {
            3 => 2,
            2 => 1,
            1 => 3,
            _ => NONE,
        };
        let mut cur = 3u32;
        let mut steps = 0usize;
        while cur != NONE && steps < COMMIT_SLOTS as usize {
            cur = parent_of(cur);
            steps += 1;
        }
        assert_eq!(steps, COMMIT_SLOTS as usize, "the bound is what stops it");
    }
}
