//! # dezh-ipc — Step 6: user-space actor IPC spike
//!
//! Dezh's final shape is microkernel-based, but we do not start with a kernel.
//! This spike validates the process model in user space: state lives behind
//! actors, messages are copied through channels, capabilities are transferred
//! only by attenuating existing grants, and actor crashes do not corrupt other
//! actors or tear down the whole system.

use std::collections::HashMap;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dezh_identity::{Authority, AuthorityGrant, Principal, Scope};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActorId(u64);

impl ActorId {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferredGrant {
    grant: AuthorityGrant,
}

impl TransferredGrant {
    pub fn grant(&self) -> &AuthorityGrant {
        &self.grant
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub from: ActorId,
    pub body: Vec<u8>,
    pub grant: Option<TransferredGrant>,
}

pub struct ActorContext {
    id: ActorId,
    inbox: Receiver<Message>,
    outboxes: HashMap<ActorId, Sender<Message>>,
    principal: Principal,
    held_grants: Vec<AuthorityGrant>,
}

impl ActorContext {
    pub fn id(&self) -> ActorId {
        self.id
    }

    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    pub fn held_grants(&self) -> &[AuthorityGrant] {
        &self.held_grants
    }

    pub fn recv(&mut self) -> std::result::Result<Message, IpcError> {
        let msg = self.inbox.recv().map_err(|_| IpcError::InboxClosed)?;
        if let Some(grant) = &msg.grant {
            self.held_grants.push(grant.grant.clone());
        }
        Ok(msg)
    }

    pub fn recv_timeout(&mut self, timeout: Duration) -> std::result::Result<Message, IpcError> {
        let msg = self.inbox.recv_timeout(timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => IpcError::Timeout,
            RecvTimeoutError::Disconnected => IpcError::InboxClosed,
        })?;
        if let Some(grant) = &msg.grant {
            self.held_grants.push(grant.grant.clone());
        }
        Ok(msg)
    }

    pub fn send(&self, to: ActorId, body: impl Into<Vec<u8>>) -> std::result::Result<(), IpcError> {
        self.send_message(to, body.into(), None)
    }

    fn send_message(
        &self,
        to: ActorId,
        body: Vec<u8>,
        grant: Option<TransferredGrant>,
    ) -> std::result::Result<(), IpcError> {
        let outbox = self.outboxes.get(&to).ok_or(IpcError::NoSuchActor)?;
        outbox
            .send(Message {
                from: self.id,
                body,
                grant,
            })
            .map_err(|_| IpcError::ActorStopped)
    }
}

/// ActorContext cannot look up target principals by itself after construction
/// unless the system injects a directory. Keep it separate to avoid exposing
/// mutation paths on the context.
struct ActorContextBuilder {
    id: ActorId,
    inbox: Receiver<Message>,
    outboxes: HashMap<ActorId, Sender<Message>>,
    principals: HashMap<ActorId, Principal>,
    principal: Principal,
    held_grants: Vec<AuthorityGrant>,
}

impl ActorContextBuilder {
    fn build(self) -> ActorContextWithDirectory {
        ActorContextWithDirectory {
            ctx: ActorContext {
                id: self.id,
                inbox: self.inbox,
                outboxes: self.outboxes,
                principal: self.principal,
                held_grants: self.held_grants,
            },
            principals: self.principals,
        }
    }
}

pub struct ActorContextWithDirectory {
    ctx: ActorContext,
    principals: HashMap<ActorId, Principal>,
}

impl ActorContextWithDirectory {
    pub fn id(&self) -> ActorId {
        self.ctx.id()
    }

    pub fn principal(&self) -> &Principal {
        self.ctx.principal()
    }

    pub fn held_grants(&self) -> &[AuthorityGrant] {
        self.ctx.held_grants()
    }

    pub fn recv(&mut self) -> std::result::Result<Message, IpcError> {
        self.ctx.recv()
    }

    pub fn recv_timeout(&mut self, timeout: Duration) -> std::result::Result<Message, IpcError> {
        self.ctx.recv_timeout(timeout)
    }

    pub fn send(&self, to: ActorId, body: impl Into<Vec<u8>>) -> std::result::Result<(), IpcError> {
        self.ctx.send(to, body)
    }

    pub fn delegate_to(
        &self,
        to: ActorId,
        parent_grant_index: usize,
        requested_scope: Scope,
        requested_authority: Authority,
        reason: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) -> std::result::Result<(), IpcError> {
        let parent = self
            .ctx
            .held_grants
            .get(parent_grant_index)
            .ok_or(IpcError::NoSuchGrant)?;
        let target = self.principals.get(&to).ok_or(IpcError::NoSuchActor)?;
        let child = parent
            .delegate(target.clone(), requested_scope, requested_authority, reason)
            .map_err(|e| IpcError::Identity(e.to_string()))?;
        self.ctx
            .send_message(to, body.into(), Some(TransferredGrant { grant: child }))
    }
}

pub struct ActorSystem {
    next_id: u64,
    senders: HashMap<ActorId, Sender<Message>>,
    principals: HashMap<ActorId, Principal>,
    handles: HashMap<ActorId, JoinHandle<ActorExit>>,
}

impl ActorSystem {
    pub fn new() -> Self {
        ActorSystem {
            next_id: 0,
            senders: HashMap::new(),
            principals: HashMap::new(),
            handles: HashMap::new(),
        }
    }

    pub fn spawn(
        &mut self,
        principal: Principal,
        initial_grants: Vec<AuthorityGrant>,
        f: impl FnOnce(ActorContextWithDirectory) + Send + 'static,
    ) -> ActorId {
        let id = ActorId(self.next_id);
        self.next_id += 1;
        let (tx, rx) = mpsc::channel();
        self.senders.insert(id, tx);
        self.principals.insert(id, principal);

        let outboxes = self.senders.clone();
        let principals = self.principals.clone();
        let principal = self
            .principals
            .get(&id)
            .expect("principal inserted above")
            .clone();
        let ctx = ActorContextBuilder {
            id,
            inbox: rx,
            outboxes,
            principals,
            principal,
            held_grants: initial_grants,
        }
        .build();
        let handle =
            thread::spawn(
                move || match panic::catch_unwind(AssertUnwindSafe(|| f(ctx))) {
                    Ok(()) => ActorExit::Completed,
                    Err(_) => ActorExit::Panicked,
                },
            );
        self.handles.insert(id, handle);
        id
    }

    pub fn send(
        &self,
        from: ActorId,
        to: ActorId,
        body: impl Into<Vec<u8>>,
    ) -> std::result::Result<(), IpcError> {
        let sender = self.senders.get(&to).ok_or(IpcError::NoSuchActor)?;
        sender
            .send(Message {
                from,
                body: body.into(),
                grant: None,
            })
            .map_err(|_| IpcError::ActorStopped)
    }

    pub fn join(&mut self, id: ActorId) -> std::result::Result<ActorExit, IpcError> {
        let handle = self.handles.remove(&id).ok_or(IpcError::NoSuchActor)?;
        let exit = handle.join().map_err(|_| IpcError::JoinFailed)?;
        self.senders.remove(&id);
        self.principals.remove(&id);
        Ok(exit)
    }
}

impl Default for ActorSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorExit {
    Completed,
    Panicked,
}

#[derive(Debug, PartialEq, Eq)]
pub enum IpcError {
    NoSuchActor,
    NoSuchGrant,
    ActorStopped,
    InboxClosed,
    Timeout,
    JoinFailed,
    Identity(String),
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpcError::NoSuchActor => write!(f, "no such actor"),
            IpcError::NoSuchGrant => write!(f, "no such grant"),
            IpcError::ActorStopped => write!(f, "actor stopped"),
            IpcError::InboxClosed => write!(f, "inbox closed"),
            IpcError::Timeout => write!(f, "receive timed out"),
            IpcError::JoinFailed => write!(f, "failed to join actor thread"),
            IpcError::Identity(e) => write!(f, "identity error: {e}"),
        }
    }
}

impl std::error::Error for IpcError {}

#[cfg(test)]
mod tests {
    use super::*;
    use dezh_identity::{PrincipalKind, Scope};
    use std::sync::mpsc;

    fn principal(name: &str) -> Principal {
        Principal::new(PrincipalKind::Agent, name).unwrap()
    }

    fn root_grant(holder: Principal) -> AuthorityGrant {
        AuthorityGrant::root(
            holder,
            Scope::new("refs/projects/dezh").unwrap(),
            Authority::READ_OBJECT
                .union(Authority::UPDATE_REF)
                .union(Authority::DELEGATE),
        )
        .unwrap()
    }

    #[test]
    fn actors_exchange_messages_without_shared_state() {
        let mut sys = ActorSystem::new();
        let (done_tx, done_rx) = mpsc::channel();
        let receiver = sys.spawn(principal("receiver"), Vec::new(), move |mut ctx| {
            let msg = ctx.recv().unwrap();
            assert_eq!(msg.body, b"hello");
            done_tx.send(msg.from).unwrap();
        });
        let sender = sys.spawn(principal("sender"), Vec::new(), move |ctx| {
            ctx.send(receiver, b"hello".to_vec()).unwrap();
        });

        let from = done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(from, sender);
        assert_eq!(sys.join(sender).unwrap(), ActorExit::Completed);
        assert_eq!(sys.join(receiver).unwrap(), ActorExit::Completed);
    }

    #[test]
    fn capability_transfer_is_attenuated() {
        let mut sys = ActorSystem::new();
        let sender_principal = principal("sender");
        let grant = root_grant(sender_principal.clone());
        let (done_tx, done_rx) = mpsc::channel();
        let receiver = sys.spawn(principal("receiver"), Vec::new(), move |mut ctx| {
            let msg = ctx.recv().unwrap();
            assert_eq!(msg.body, b"grant");
            assert_eq!(ctx.held_grants().len(), 1);
            let grant = &ctx.held_grants()[0];
            assert_eq!(grant.scope().as_str(), "refs/projects/dezh/docs");
            assert!(grant.authority().contains(Authority::READ_OBJECT));
            assert!(!grant.authority().contains(Authority::UPDATE_REF));
            done_tx.send(()).unwrap();
        });
        let sender = sys.spawn(sender_principal, vec![grant], move |ctx| {
            ctx.delegate_to(
                receiver,
                0,
                Scope::new("refs/projects/dezh/docs").unwrap(),
                Authority::READ_OBJECT,
                "share read-only docs",
                b"grant".to_vec(),
            )
            .unwrap();
        });

        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(sys.join(sender).unwrap(), ActorExit::Completed);
        assert_eq!(sys.join(receiver).unwrap(), ActorExit::Completed);
    }

    #[test]
    fn transfer_cannot_widen_authority() {
        let mut sys = ActorSystem::new();
        let sender_principal = principal("sender");
        let grant = AuthorityGrant::root(
            sender_principal.clone(),
            Scope::new("refs/projects/dezh").unwrap(),
            Authority::READ_OBJECT.union(Authority::DELEGATE),
        )
        .unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let receiver = sys.spawn(principal("receiver"), Vec::new(), |_ctx| {});
        let sender = sys.spawn(sender_principal, vec![grant], move |ctx| {
            let err = ctx
                .delegate_to(
                    receiver,
                    0,
                    Scope::new("refs/projects/dezh/docs").unwrap(),
                    Authority::READ_OBJECT.union(Authority::UPDATE_REF),
                    "try widen",
                    b"grant".to_vec(),
                )
                .unwrap_err();
            assert!(matches!(err, IpcError::Identity(_)));
            done_tx.send(()).unwrap();
        });

        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(sys.join(sender).unwrap(), ActorExit::Completed);
        assert_eq!(sys.join(receiver).unwrap(), ActorExit::Completed);
    }

    #[test]
    fn panicking_actor_is_isolated() {
        let mut sys = ActorSystem::new();
        let (done_tx, done_rx) = mpsc::channel();
        let survivor = sys.spawn(principal("survivor"), Vec::new(), move |mut ctx| {
            let msg = ctx.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(msg.body, b"still alive");
            done_tx.send(()).unwrap();
        });
        let crashing = sys.spawn(principal("crashing"), Vec::new(), |_ctx| {
            panic!("actor crash");
        });

        assert_eq!(sys.join(crashing).unwrap(), ActorExit::Panicked);
        sys.send(ActorId(999), survivor, b"still alive".to_vec())
            .unwrap();
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(sys.join(survivor).unwrap(), ActorExit::Completed);
    }
}
