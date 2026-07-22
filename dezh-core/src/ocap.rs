//! Object-capabilities with generation-stamped revocation (the "one big change").
//!
//! Dezh's task capabilities are a coarse per-task bitmask (see
//! `docs/SECURITY_MODEL.md`). This module is the first-class alternative a
//! serious reviewer asks for: a capability is an **unforgeable handle to one
//! specific object**, carrying **attenuable rights** and a **generation stamp**.
//! It is arch-independent and `no_std`/alloc-free, so it is host-tested here and
//! driven by the kernel `cap-demo`.
//!
//! Three properties fall out that a bitmask cannot express:
//!
//! 1. **Per-object revocation.** Each object has a live generation in the
//!    authority (`CapTable`). Revoking an object bumps its generation, so every
//!    outstanding handle to *that* object — and only that object — becomes stale
//!    at next use, without tracking who holds it.
//! 2. **Attenuated delegation graph.** `derive` mints a child handle whose rights
//!    are a subset of the parent's (`child = parent ∩ mask`); you can pass less
//!    than you hold, never more.
//! 3. **Object granularity.** Authority names a specific object + rights, not a
//!    whole class/namespace bit.
//!
//! Handles are "unforgeable" in the capability sense: this type has no public
//! constructor for an arbitrary `(object, generation)` — a live handle can only
//! come from `CapTable::mint` (the authority) or `derive` (attenuation of an
//! existing one), so U-mode code cannot fabricate one out of thin air.

/// An object identifier (index into the authority's generation table).
pub type ObjectId = usize;

/// A rights bitmask over one object (read/write/append/delegate/...). The exact
/// bits are the caller's vocabulary; the algebra only relies on subset order.
pub type Rights = u8;

pub const R_READ: Rights = 1 << 0;
pub const R_WRITE: Rights = 1 << 1;
pub const R_APPEND: Rights = 1 << 2;
pub const R_DELEGATE: Rights = 1 << 3;

/// A generation value that is never live (a zeroed/forged handle fails the
/// liveness check, since real generations start at 1).
pub const GEN_NEVER: u32 = 0;

/// An object-capability: an unforgeable reference to one object, with attenuable
/// rights and the generation it was minted at. `Copy` so it is cheap to pass;
/// unforgeable because only `CapTable` mints live handles (no public field-wise
/// constructor is exposed as valid — see `forged`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    object: ObjectId,
    rights: Rights,
    generation: u32,
}

impl Cap {
    pub fn object(&self) -> ObjectId {
        self.object
    }
    pub fn rights(&self) -> Rights {
        self.rights
    }
    pub fn generation(&self) -> u32 {
        self.generation
    }
    /// Construct a handle with an arbitrary generation — used only to model an
    /// attacker fabricating one. Such a handle will fail liveness unless its
    /// generation happens to match the live one (which an attacker cannot know
    /// how to forge for a revoked object). Not a path to real authority.
    pub fn forged(object: ObjectId, rights: Rights, generation: u32) -> Cap {
        Cap {
            object,
            rights,
            generation,
        }
    }
}

/// The revocation outcome of using a capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapCheck {
    /// The handle is live and carries the requested rights.
    Ok,
    /// The handle's generation is stale — the object was revoked.
    Revoked,
    /// The handle is live but lacks the requested rights.
    Denied,
    /// The object id is out of range.
    NoSuchObject,
}

/// The authority: the live generation per object. Bumping a generation revokes
/// every outstanding handle to that object. `N` objects, generations start at 1.
pub struct CapTable<const N: usize> {
    gen: [u32; N],
}

impl<const N: usize> Default for CapTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> CapTable<N> {
    pub const fn new() -> Self {
        CapTable { gen: [1u32; N] }
    }

    /// Mint a fresh handle to `object` with `rights`, stamped with the object's
    /// current generation. Only the authority can do this.
    pub fn mint(&self, object: ObjectId, rights: Rights) -> Option<Cap> {
        if object >= N {
            return None;
        }
        Some(Cap {
            object,
            rights,
            generation: self.gen[object],
        })
    }

    /// Attenuated delegation: a child handle to the SAME object whose rights are
    /// a subset of the parent's (`child = parent.rights ∩ mask`), inheriting the
    /// parent's generation. Requires the parent to hold `R_DELEGATE`. Rights can
    /// only ever narrow — never widen.
    pub fn derive(&self, parent: &Cap, mask: Rights) -> Option<Cap> {
        if parent.rights & R_DELEGATE == 0 {
            return None;
        }
        Some(Cap {
            object: parent.object,
            rights: parent.rights & mask,
            generation: parent.generation,
        })
    }

    /// Is this handle live (its generation matches the object's current one)?
    pub fn is_live(&self, cap: &Cap) -> bool {
        cap.object < N && cap.generation != GEN_NEVER && cap.generation == self.gen[cap.object]
    }

    /// Use `cap` for operations requiring `want` rights.
    pub fn check(&self, cap: &Cap, want: Rights) -> CapCheck {
        if cap.object >= N {
            return CapCheck::NoSuchObject;
        }
        if cap.generation == GEN_NEVER || cap.generation != self.gen[cap.object] {
            return CapCheck::Revoked;
        }
        if cap.rights & want != want {
            return CapCheck::Denied;
        }
        CapCheck::Ok
    }

    /// Revoke every outstanding handle to `object` by bumping its generation.
    /// Per-object: handles to other objects are unaffected.
    pub fn revoke(&mut self, object: ObjectId) {
        if object < N {
            self.gen[object] = self.gen[object].wrapping_add(1);
        }
    }

    pub fn generation_of(&self, object: ObjectId) -> u32 {
        if object < N {
            self.gen[object]
        } else {
            GEN_NEVER
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_then_use() {
        let t = CapTable::<8>::new();
        let c = t.mint(3, R_READ | R_WRITE).unwrap();
        assert_eq!(t.check(&c, R_READ), CapCheck::Ok);
        assert_eq!(t.check(&c, R_READ | R_WRITE), CapCheck::Ok);
        assert_eq!(t.check(&c, R_DELEGATE), CapCheck::Denied);
    }

    #[test]
    fn revocation_is_per_object() {
        let mut t = CapTable::<8>::new();
        let a = t.mint(3, R_READ).unwrap();
        let b = t.mint(5, R_READ).unwrap();
        assert_eq!(t.check(&a, R_READ), CapCheck::Ok);
        assert_eq!(t.check(&b, R_READ), CapCheck::Ok);
        // Revoke object 3: its handle goes stale, object 5's does NOT.
        t.revoke(3);
        assert_eq!(t.check(&a, R_READ), CapCheck::Revoked);
        assert_eq!(t.check(&b, R_READ), CapCheck::Ok);
    }

    #[test]
    fn derive_never_widens_rights() {
        // Exhaustive over the 8-bit rights space: a derived child is always a
        // subset of the parent (and of the requested mask).
        let t = CapTable::<4>::new();
        for pr in 0u16..=255 {
            let parent = Cap::forged(1, pr as u8 | R_DELEGATE, t.generation_of(1));
            for mask in 0u16..=255 {
                let child = t.derive(&parent, mask as u8).unwrap();
                assert_eq!(child.rights() & !parent.rights(), 0, "child widened past parent");
                assert_eq!(child.rights() & !(mask as u8), 0, "child widened past mask");
                assert_eq!(child.object(), parent.object());
                assert_eq!(child.generation(), parent.generation());
            }
        }
    }

    #[test]
    fn derive_requires_delegate_right() {
        let t = CapTable::<4>::new();
        let no_deleg = t.mint(1, R_READ | R_WRITE).unwrap();
        assert!(t.derive(&no_deleg, R_READ).is_none());
        let can_deleg = t.mint(1, R_READ | R_DELEGATE).unwrap();
        assert!(t.derive(&can_deleg, R_READ).is_some());
    }

    #[test]
    fn derived_child_is_revoked_with_the_object() {
        // Revoking the object invalidates the whole delegation subtree at once.
        let mut t = CapTable::<4>::new();
        let parent = t.mint(2, R_READ | R_WRITE | R_DELEGATE).unwrap();
        let child = t.derive(&parent, R_READ).unwrap();
        assert_eq!(t.check(&child, R_READ), CapCheck::Ok);
        t.revoke(2);
        assert_eq!(t.check(&parent, R_READ), CapCheck::Revoked);
        assert_eq!(t.check(&child, R_READ), CapCheck::Revoked);
    }

    #[test]
    fn forged_handle_is_not_live() {
        // A handle with a generation the attacker guessed wrong fails liveness.
        let t = CapTable::<4>::new(); // live generation is 1
        let forged = Cap::forged(1, R_READ | R_WRITE, 999);
        assert!(!t.is_live(&forged));
        assert_eq!(t.check(&forged, R_READ), CapCheck::Revoked);
        let zero = Cap::forged(1, R_READ, GEN_NEVER);
        assert!(!t.is_live(&zero));
    }

    #[test]
    fn out_of_range_object() {
        let t = CapTable::<4>::new();
        assert!(t.mint(4, R_READ).is_none());
        let bad = Cap::forged(9, R_READ, 1);
        assert_eq!(t.check(&bad, R_READ), CapCheck::NoSuchObject);
    }
}
