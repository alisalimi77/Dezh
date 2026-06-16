//! # dezh-identity — Step 3: identity, delegation, and provenance
//!
//! The capability core answers "what authority exists?". Step 3 answers the
//! missing agent-era questions: who held the authority, on whose behalf, how it
//! was attenuated, and what action it produced.
//!
//! This crate is deliberately not wired into Cairn yet. Step 4 will connect
//! these invocation records to Cairn commits. Here we validate the identity
//! model in isolation: delegation can only narrow authority, sub-agents inherit
//! a complete delegation chain, and invocations record the exact grant used.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Stable identity for a human, service, or agent.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PrincipalId([u8; 32]);

impl PrincipalId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for PrincipalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PrincipalId({})", hex(&self.0))
    }
}

impl fmt::Display for PrincipalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex(&self.0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrincipalKind {
    Human,
    Service,
    Agent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    id: PrincipalId,
    kind: PrincipalKind,
    name: String,
}

impl Principal {
    pub fn new(kind: PrincipalKind, name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(IdentityError::InvalidPrincipal);
        }
        let mut h = blake3::Hasher::new();
        h.update(kind.tag().as_bytes());
        h.update(name.as_bytes());
        Ok(Principal {
            id: PrincipalId(*h.finalize().as_bytes()),
            kind,
            name,
        })
    }

    pub fn id(&self) -> &PrincipalId {
        &self.id
    }

    pub fn kind(&self) -> &PrincipalKind {
        &self.kind
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl PrincipalKind {
    fn tag(&self) -> &'static str {
        match self {
            PrincipalKind::Human => "human",
            PrincipalKind::Service => "service",
            PrincipalKind::Agent => "agent",
        }
    }
}

/// A compact authority set. The inner bits are private so callers can only use
/// named operations and set algebra that cannot invent undefined rights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Authority(u32);

impl Authority {
    pub const READ_OBJECT: Authority = Authority(1 << 0);
    pub const READ_REF: Authority = Authority(1 << 1);
    pub const UPDATE_REF: Authority = Authority(1 << 2);
    pub const ROLLBACK_REF: Authority = Authority(1 << 3);
    pub const DELEGATE: Authority = Authority(1 << 4);
    pub const NONE: Authority = Authority(0);

    const ALL_BITS: u32 = Self::READ_OBJECT.0
        | Self::READ_REF.0
        | Self::UPDATE_REF.0
        | Self::ROLLBACK_REF.0
        | Self::DELEGATE.0;

    pub fn from_bits_truncate(raw: u32) -> Self {
        Authority(raw & Self::ALL_BITS)
    }

    pub fn bits(self) -> u32 {
        self.0
    }

    pub fn contains(self, other: Authority) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Authority) -> Authority {
        Authority(self.0 | other.0)
    }

    pub fn intersect(self, other: Authority) -> Authority {
        Authority(self.0 & other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// A resource scope. v0 treats scopes as slash-separated names. A child scope is
/// narrower when it is equal to the parent or starts with `parent/`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Scope(String);

impl Scope {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.starts_with('/') || value.ends_with('/') {
            return Err(IdentityError::InvalidScope);
        }
        Ok(Scope(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn contains(&self, child: &Scope) -> bool {
        child.0 == self.0
            || child
                .0
                .strip_prefix(&self.0)
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// One link in a delegated authority chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DelegationLink {
    pub from: PrincipalId,
    pub to: PrincipalId,
    pub scope: Scope,
    pub authority: Authority,
    pub reason: String,
    pub timestamp_millis: u128,
}

/// The authority a principal currently holds. Fields are private: outside code
/// can inspect grants and ask to delegate/invoke, but it cannot mutate a grant
/// into a wider one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorityGrant {
    holder: Principal,
    scope: Scope,
    authority: Authority,
    chain: Vec<DelegationLink>,
}

impl AuthorityGrant {
    /// Trusted host minting of root authority. Later kernel/runtime phases will
    /// restrict this path to trusted system code.
    pub fn root(holder: Principal, scope: Scope, authority: Authority) -> Result<Self> {
        if authority.is_empty() {
            return Err(IdentityError::EmptyAuthority);
        }
        Ok(AuthorityGrant {
            holder,
            scope,
            authority,
            chain: Vec::new(),
        })
    }

    pub fn holder(&self) -> &Principal {
        &self.holder
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn authority(&self) -> Authority {
        self.authority
    }

    pub fn chain(&self) -> &[DelegationLink] {
        &self.chain
    }

    pub fn delegate(
        &self,
        child: Principal,
        requested_scope: Scope,
        requested_authority: Authority,
        reason: impl Into<String>,
    ) -> Result<AuthorityGrant> {
        let reason = reason.into();
        if reason.is_empty() {
            return Err(IdentityError::InvalidReason);
        }
        if requested_authority.is_empty() {
            return Err(IdentityError::EmptyAuthority);
        }
        if !self.authority.contains(Authority::DELEGATE) {
            return Err(IdentityError::DelegationNotPermitted);
        }
        if !self.authority.contains(requested_authority) {
            return Err(IdentityError::AuthorityWidening);
        }
        if !self.scope.contains(&requested_scope) {
            return Err(IdentityError::ScopeWidening);
        }

        let effective = self.authority.intersect(requested_authority);
        let link = DelegationLink {
            from: self.holder.id.clone(),
            to: child.id.clone(),
            scope: requested_scope.clone(),
            authority: effective,
            reason,
            timestamp_millis: now_millis(),
        };
        let mut chain = self.chain.clone();
        chain.push(link);
        Ok(AuthorityGrant {
            holder: child,
            scope: requested_scope,
            authority: effective,
            chain,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactKind {
    Object,
    Commit,
    Ref,
    Message,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub kind: ArtifactKind,
    pub id: String,
}

impl Artifact {
    pub fn new(kind: ArtifactKind, id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        if id.is_empty() {
            return Err(IdentityError::InvalidArtifact);
        }
        Ok(Artifact { kind, id })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub id: InvocationId,
    pub actor: PrincipalId,
    pub scope: Scope,
    pub used_authority: Authority,
    pub action: String,
    pub reason: String,
    pub outputs: Vec<Artifact>,
    pub delegation_chain: Vec<DelegationLink>,
    pub timestamp_millis: u128,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct InvocationId([u8; 32]);

impl InvocationId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for InvocationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InvocationId({})", hex(&self.0))
    }
}

impl fmt::Display for InvocationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex(&self.0))
    }
}

impl Invocation {
    pub fn record(
        grant: &AuthorityGrant,
        required_authority: Authority,
        action: impl Into<String>,
        reason: impl Into<String>,
        outputs: Vec<Artifact>,
    ) -> Result<Self> {
        let action = action.into();
        let reason = reason.into();
        if action.is_empty() || reason.is_empty() {
            return Err(IdentityError::InvalidReason);
        }
        if outputs.is_empty() {
            return Err(IdentityError::NoOutputs);
        }
        if !grant.authority.contains(required_authority) {
            return Err(IdentityError::AuthorityNotHeld);
        }

        let timestamp_millis = now_millis();
        let id = invocation_id(
            grant.holder.id(),
            grant.scope(),
            required_authority,
            &action,
            &reason,
            &outputs,
            &grant.chain,
            timestamp_millis,
        );
        Ok(Invocation {
            id,
            actor: grant.holder.id.clone(),
            scope: grant.scope.clone(),
            used_authority: required_authority,
            action,
            reason,
            outputs,
            delegation_chain: grant.chain.clone(),
            timestamp_millis,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum IdentityError {
    InvalidPrincipal,
    InvalidScope,
    InvalidReason,
    InvalidArtifact,
    EmptyAuthority,
    DelegationNotPermitted,
    AuthorityWidening,
    ScopeWidening,
    AuthorityNotHeld,
    NoOutputs,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::InvalidPrincipal => write!(f, "principal must have a name"),
            IdentityError::InvalidScope => write!(f, "scope must be a non-empty relative path"),
            IdentityError::InvalidReason => write!(f, "action/delegation reason must be non-empty"),
            IdentityError::InvalidArtifact => write!(f, "artifact id must be non-empty"),
            IdentityError::EmptyAuthority => write!(f, "authority set must be non-empty"),
            IdentityError::DelegationNotPermitted => write!(f, "grant lacks DELEGATE authority"),
            IdentityError::AuthorityWidening => {
                write!(f, "delegation requested authority the parent did not hold")
            }
            IdentityError::ScopeWidening => {
                write!(f, "delegation requested a scope outside the parent scope")
            }
            IdentityError::AuthorityNotHeld => {
                write!(f, "invocation requested authority the grant does not hold")
            }
            IdentityError::NoOutputs => write!(f, "invocation must produce at least one artifact"),
        }
    }
}

impl std::error::Error for IdentityError {}

pub type Result<T> = std::result::Result<T, IdentityError>;

fn invocation_id(
    actor: &PrincipalId,
    scope: &Scope,
    required_authority: Authority,
    action: &str,
    reason: &str,
    outputs: &[Artifact],
    chain: &[DelegationLink],
    timestamp_millis: u128,
) -> InvocationId {
    let mut h = blake3::Hasher::new();
    h.update(actor.as_bytes());
    h.update(scope.as_str().as_bytes());
    h.update(&required_authority.bits().to_le_bytes());
    h.update(action.as_bytes());
    h.update(reason.as_bytes());
    for output in outputs {
        h.update(output.kind.tag().as_bytes());
        h.update(output.id.as_bytes());
    }
    for link in chain {
        h.update(link.from.as_bytes());
        h.update(link.to.as_bytes());
        h.update(link.scope.as_str().as_bytes());
        h.update(&link.authority.bits().to_le_bytes());
        h.update(link.reason.as_bytes());
        h.update(&link.timestamp_millis.to_le_bytes());
    }
    h.update(&timestamp_millis.to_le_bytes());
    InvocationId(*h.finalize().as_bytes())
}

impl ArtifactKind {
    fn tag(&self) -> &'static str {
        match self {
            ArtifactKind::Object => "object",
            ArtifactKind::Commit => "commit",
            ArtifactKind::Ref => "ref",
            ArtifactKind::Message => "message",
        }
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human_root() -> AuthorityGrant {
        let human = Principal::new(PrincipalKind::Human, "ali").unwrap();
        AuthorityGrant::root(
            human,
            Scope::new("refs/projects/dezh").unwrap(),
            Authority::READ_OBJECT
                .union(Authority::READ_REF)
                .union(Authority::UPDATE_REF)
                .union(Authority::ROLLBACK_REF)
                .union(Authority::DELEGATE),
        )
        .unwrap()
    }

    #[test]
    fn delegation_can_only_narrow_authority() {
        let root = human_root();
        let agent = Principal::new(PrincipalKind::Agent, "writer").unwrap();

        let grant = root
            .delegate(
                agent,
                Scope::new("refs/projects/dezh/docs").unwrap(),
                Authority::READ_OBJECT.union(Authority::UPDATE_REF),
                "edit docs only",
            )
            .unwrap();

        assert!(grant.authority().contains(Authority::READ_OBJECT));
        assert!(grant.authority().contains(Authority::UPDATE_REF));
        assert!(!grant.authority().contains(Authority::ROLLBACK_REF));
        assert!(!grant.authority().contains(Authority::DELEGATE));
        assert_eq!(grant.scope().as_str(), "refs/projects/dezh/docs");
        assert_eq!(grant.chain().len(), 1);
    }

    #[test]
    fn delegation_requires_delegate_authority() {
        // A grant without DELEGATE cannot sub-delegate at all — it fails before
        // any widening check is even reached.
        let root = human_root();
        let agent = Principal::new(PrincipalKind::Agent, "rollbacker").unwrap();
        let narrow = root
            .delegate(
                agent.clone(),
                Scope::new("refs/projects/dezh/docs").unwrap(),
                Authority::READ_OBJECT,
                "read docs",
            )
            .unwrap();

        let err = narrow
            .delegate(
                agent,
                Scope::new("refs/projects/dezh/docs").unwrap(),
                Authority::READ_OBJECT,
                "cannot sub-delegate without DELEGATE",
            )
            .unwrap_err();

        assert_eq!(err, IdentityError::DelegationNotPermitted);
    }

    #[test]
    fn delegation_rejects_authority_widening() {
        // The grant CAN delegate (holds DELEGATE) but holds only READ_OBJECT, so
        // requesting UPDATE_REF must be rejected as widening — this exercises the
        // widening branch itself, not the missing-DELEGATE branch.
        let root = human_root();
        let mid_agent = Principal::new(PrincipalKind::Agent, "mid").unwrap();
        let sub_agent = Principal::new(PrincipalKind::Agent, "sub").unwrap();
        let mid = root
            .delegate(
                mid_agent,
                Scope::new("refs/projects/dezh/docs").unwrap(),
                Authority::READ_OBJECT.union(Authority::DELEGATE),
                "mid may sub-delegate reads",
            )
            .unwrap();

        let err = mid
            .delegate(
                sub_agent,
                Scope::new("refs/projects/dezh/docs").unwrap(),
                Authority::READ_OBJECT.union(Authority::UPDATE_REF),
                "try to widen beyond held authority",
            )
            .unwrap_err();

        assert_eq!(err, IdentityError::AuthorityWidening);
    }

    #[test]
    fn delegation_rejects_scope_widening() {
        let root = human_root();
        let agent = Principal::new(PrincipalKind::Agent, "wide").unwrap();

        let err = root
            .delegate(
                agent,
                Scope::new("refs/projects/other").unwrap(),
                Authority::READ_OBJECT,
                "reach outside scope",
            )
            .unwrap_err();

        assert_eq!(err, IdentityError::ScopeWidening);
    }

    #[test]
    fn sub_agent_gets_complete_delegation_chain() {
        let root = human_root();
        let writer = Principal::new(PrincipalKind::Agent, "writer").unwrap();
        let reviewer = Principal::new(PrincipalKind::Agent, "reviewer").unwrap();
        let writer_grant = root
            .delegate(
                writer,
                Scope::new("refs/projects/dezh/docs").unwrap(),
                Authority::READ_OBJECT
                    .union(Authority::UPDATE_REF)
                    .union(Authority::DELEGATE),
                "writer can assign review",
            )
            .unwrap();

        let reviewer_grant = writer_grant
            .delegate(
                reviewer,
                Scope::new("refs/projects/dezh/docs/review").unwrap(),
                Authority::READ_OBJECT,
                "review subset",
            )
            .unwrap();

        assert_eq!(reviewer_grant.chain().len(), 2);
        assert_eq!(reviewer_grant.chain()[0].reason, "writer can assign review");
        assert_eq!(reviewer_grant.chain()[1].reason, "review subset");
    }

    #[test]
    fn invocation_records_actor_authority_outputs_and_chain() {
        let root = human_root();
        let agent = Principal::new(PrincipalKind::Agent, "writer").unwrap();
        let grant = root
            .delegate(
                agent.clone(),
                Scope::new("refs/projects/dezh/docs").unwrap(),
                Authority::READ_OBJECT.union(Authority::UPDATE_REF),
                "edit docs only",
            )
            .unwrap();

        let invocation = Invocation::record(
            &grant,
            Authority::UPDATE_REF,
            "cairn.commit",
            "write architecture note",
            vec![
                Artifact::new(ArtifactKind::Object, "object:abc").unwrap(),
                Artifact::new(ArtifactKind::Commit, "commit:def").unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(&invocation.actor, agent.id());
        assert_eq!(invocation.scope.as_str(), "refs/projects/dezh/docs");
        assert_eq!(invocation.used_authority, Authority::UPDATE_REF);
        assert_eq!(invocation.outputs.len(), 2);
        assert_eq!(invocation.delegation_chain.len(), 1);
        assert_eq!(invocation.delegation_chain[0].reason, "edit docs only");
    }

    #[test]
    fn invocation_rejects_authority_not_held() {
        let root = human_root();
        let agent = Principal::new(PrincipalKind::Agent, "reader").unwrap();
        let grant = root
            .delegate(
                agent,
                Scope::new("refs/projects/dezh/docs").unwrap(),
                Authority::READ_OBJECT,
                "read only",
            )
            .unwrap();

        let err = Invocation::record(
            &grant,
            Authority::UPDATE_REF,
            "cairn.commit",
            "try write",
            vec![Artifact::new(ArtifactKind::Commit, "commit:def").unwrap()],
        )
        .unwrap_err();

        assert_eq!(err, IdentityError::AuthorityNotHeld);
    }
}
