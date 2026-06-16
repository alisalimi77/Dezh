//! # dezh-linux - Step 9: Linux personality server spike
//!
//! This is not an ELF loader and not a complete Linux ABI. It is the first
//! compatibility bridge spike: a user-space Linux-like personality server that
//! exposes a virtual filesystem view to legacy code while enforcing Dezh
//! capabilities before touching Cairn.
//!
//! A legacy path such as `/home/app/data.txt` is mapped through an explicit
//! mount to a Cairn ref such as `refs/legacy/home/data.txt`. The guest sees a
//! normal path, but the server only exposes refs covered by an `AuthorityGrant`.

use std::collections::HashMap;
use std::fmt;

use dezh_cairn::{CairnStore, CommitId, ObjectId};
use dezh_identity::{Artifact, ArtifactKind, Authority, AuthorityGrant, Invocation, Scope};

pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1 << 0;
pub const O_RDWR: u32 = 1 << 1;
pub const O_CREAT: u32 = 1 << 2;
pub const O_TRUNC: u32 = 1 << 3;

pub type Fd = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxErrno {
    Eacces,
    Ebadf,
    Eexist,
    Einval,
    Enoent,
    Enosys,
}

impl LinuxErrno {
    pub fn code(self) -> i32 {
        match self {
            LinuxErrno::Eacces => 13,
            LinuxErrno::Ebadf => 9,
            LinuxErrno::Eexist => 17,
            LinuxErrno::Einval => 22,
            LinuxErrno::Enoent => 2,
            LinuxErrno::Enosys => 38,
        }
    }
}

impl fmt::Display for LinuxErrno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinuxErrno::Eacces => write!(f, "permission denied"),
            LinuxErrno::Ebadf => write!(f, "bad file descriptor"),
            LinuxErrno::Eexist => write!(f, "file exists"),
            LinuxErrno::Einval => write!(f, "invalid argument"),
            LinuxErrno::Enoent => write!(f, "no such file or directory"),
            LinuxErrno::Enosys => write!(f, "syscall not implemented"),
        }
    }
}

impl std::error::Error for LinuxErrno {}

pub type Result<T> = std::result::Result<T, LinuxErrno>;

#[derive(Clone, Debug)]
pub struct Mount {
    guest_prefix: String,
    ref_prefix: String,
    grant: AuthorityGrant,
}

impl Mount {
    pub fn new(
        guest_prefix: impl Into<String>,
        ref_prefix: impl Into<String>,
        grant: AuthorityGrant,
    ) -> Result<Self> {
        let guest_prefix = normalize_guest_path(&guest_prefix.into())?;
        let ref_prefix = normalize_ref_prefix(&ref_prefix.into())?;
        let ref_scope = Scope::new(ref_prefix.clone()).map_err(|_| LinuxErrno::Einval)?;
        if !grant.scope().contains(&ref_scope) {
            return Err(LinuxErrno::Eacces);
        }
        Ok(Mount {
            guest_prefix,
            ref_prefix,
            grant,
        })
    }

    pub fn guest_prefix(&self) -> &str {
        &self.guest_prefix
    }

    pub fn ref_prefix(&self) -> &str {
        &self.ref_prefix
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFile {
    pub ref_name: String,
    pub readable: bool,
    pub writable: bool,
    position: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteReceipt {
    pub bytes_written: usize,
    pub object: ObjectId,
    pub commit: CommitId,
}

pub struct LinuxPersonality {
    cairn: CairnStore,
    mounts: Vec<Mount>,
    files: HashMap<Fd, OpenFile>,
    next_fd: Fd,
    invocations: Vec<Invocation>,
}

impl LinuxPersonality {
    pub fn new(cairn: CairnStore) -> Self {
        LinuxPersonality {
            cairn,
            mounts: Vec::new(),
            files: HashMap::new(),
            next_fd: 3,
            invocations: Vec::new(),
        }
    }

    pub fn cairn(&self) -> &CairnStore {
        &self.cairn
    }

    pub fn cairn_mut(&mut self) -> &mut CairnStore {
        &mut self.cairn
    }

    pub fn invocations(&self) -> &[Invocation] {
        &self.invocations
    }

    pub fn mount(&mut self, mount: Mount) {
        self.mounts.push(mount);
        self.mounts
            .sort_by(|a, b| b.guest_prefix.len().cmp(&a.guest_prefix.len()));
    }

    pub fn open(&mut self, path: &str, flags: u32) -> Result<Fd> {
        let access = AccessMode::from_flags(flags)?;
        let (ref_name, grant) = self.resolve(path)?;
        let ref_scope = Scope::new(ref_name.clone()).map_err(|_| LinuxErrno::Einval)?;
        if !grant.scope().contains(&ref_scope) {
            return Err(LinuxErrno::Eacces);
        }

        if access.readable()
            && !grant
                .authority()
                .contains(Authority::READ_REF.union(Authority::READ_OBJECT))
        {
            return Err(LinuxErrno::Eacces);
        }
        if access.writable() && !grant.authority().contains(Authority::UPDATE_REF) {
            return Err(LinuxErrno::Eacces);
        }

        let exists = self.cairn.get_ref(&ref_name).is_some();
        if !exists && !flags_has(flags, O_CREAT) {
            return Err(LinuxErrno::Enoent);
        }
        if flags_has(flags, O_TRUNC) && !access.writable() {
            return Err(LinuxErrno::Einval);
        }
        if flags_has(flags, O_TRUNC) {
            self.commit_bytes(&ref_name, &grant, b"", "linux.open.truncate")?;
        } else if !exists && flags_has(flags, O_CREAT) {
            self.commit_bytes(&ref_name, &grant, b"", "linux.open.create")?;
        }

        let fd = self.next_fd;
        self.next_fd = self.next_fd.checked_add(1).ok_or(LinuxErrno::Einval)?;
        self.files.insert(
            fd,
            OpenFile {
                ref_name,
                readable: access.readable(),
                writable: access.writable(),
                position: 0,
            },
        );
        Ok(fd)
    }

    pub fn read(&mut self, fd: Fd, out: &mut [u8]) -> Result<usize> {
        let file = self.files.get_mut(&fd).ok_or(LinuxErrno::Ebadf)?;
        if !file.readable {
            return Err(LinuxErrno::Ebadf);
        }
        let object = self
            .cairn
            .get_ref(&file.ref_name)
            .ok_or(LinuxErrno::Enoent)?;
        let bytes = self.cairn.get(object).ok_or(LinuxErrno::Enoent)?;
        let start = file.position.min(bytes.len());
        let n = out.len().min(bytes.len().saturating_sub(start));
        out[..n].copy_from_slice(&bytes[start..start + n]);
        file.position += n;
        Ok(n)
    }

    pub fn write(&mut self, fd: Fd, bytes: &[u8]) -> Result<WriteReceipt> {
        let file = self.files.get(&fd).ok_or(LinuxErrno::Ebadf)?.clone();
        if !file.writable {
            return Err(LinuxErrno::Ebadf);
        }
        let (_, grant) = self
            .mount_for_ref(&file.ref_name)
            .ok_or(LinuxErrno::Eacces)?;
        let grant = grant.clone();
        self.commit_bytes(&file.ref_name, &grant, bytes, "linux.write")
    }

    pub fn close(&mut self, fd: Fd) -> Result<()> {
        self.files.remove(&fd).ok_or(LinuxErrno::Ebadf)?;
        Ok(())
    }

    pub fn unsupported_syscall(&self, _number: u64) -> Result<()> {
        Err(LinuxErrno::Enosys)
    }

    fn resolve(&self, guest_path: &str) -> Result<(String, AuthorityGrant)> {
        let normalized = normalize_guest_path(guest_path)?;
        for mount in &self.mounts {
            if let Some(suffix) = path_suffix(&normalized, &mount.guest_prefix) {
                let ref_name = if suffix.is_empty() {
                    mount.ref_prefix.clone()
                } else {
                    format!("{}/{}", mount.ref_prefix, suffix)
                };
                return Ok((ref_name, mount.grant.clone()));
            }
        }
        Err(LinuxErrno::Eacces)
    }

    fn mount_for_ref(&self, ref_name: &str) -> Option<(&Mount, &AuthorityGrant)> {
        self.mounts
            .iter()
            .find(|mount| ref_suffix(ref_name, mount.ref_prefix()).is_some())
            .map(|mount| (mount, &mount.grant))
    }

    fn commit_bytes(
        &mut self,
        ref_name: &str,
        grant: &AuthorityGrant,
        bytes: &[u8],
        action: &str,
    ) -> Result<WriteReceipt> {
        if !grant.authority().contains(Authority::UPDATE_REF) {
            return Err(LinuxErrno::Eacces);
        }
        let ref_scope = Scope::new(ref_name.to_owned()).map_err(|_| LinuxErrno::Einval)?;
        if !grant.scope().contains(&ref_scope) {
            return Err(LinuxErrno::Eacces);
        }

        let object = self.cairn.put(bytes).map_err(|_| LinuxErrno::Einval)?;
        let commit = self
            .cairn
            .begin_tx()
            .tap(|tx| tx.set_ref(ref_name, object))
            .commit(grant.holder().name(), action)
            .map_err(|_| LinuxErrno::Einval)?;
        let invocation = Invocation::record(
            grant,
            Authority::UPDATE_REF,
            action,
            "linux personality filesystem mutation",
            vec![
                Artifact::new(ArtifactKind::Object, format!("object:{object}"))
                    .map_err(|_| LinuxErrno::Einval)?,
                Artifact::new(ArtifactKind::Commit, format!("commit:{commit}"))
                    .map_err(|_| LinuxErrno::Einval)?,
            ],
        )
        .map_err(|_| LinuxErrno::Eacces)?;
        self.invocations.push(invocation);
        Ok(WriteReceipt {
            bytes_written: bytes.len(),
            object,
            commit,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccessMode {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

impl AccessMode {
    fn from_flags(flags: u32) -> Result<Self> {
        match (flags_has(flags, O_WRONLY), flags_has(flags, O_RDWR)) {
            (false, false) => Ok(AccessMode::ReadOnly),
            (true, false) => Ok(AccessMode::WriteOnly),
            (false, true) => Ok(AccessMode::ReadWrite),
            (true, true) => Err(LinuxErrno::Einval),
        }
    }

    fn readable(self) -> bool {
        matches!(self, AccessMode::ReadOnly | AccessMode::ReadWrite)
    }

    fn writable(self) -> bool {
        matches!(self, AccessMode::WriteOnly | AccessMode::ReadWrite)
    }
}

fn flags_has(flags: u32, bit: u32) -> bool {
    flags & bit == bit
}

fn normalize_guest_path(path: &str) -> Result<String> {
    if !path.starts_with('/') {
        return Err(LinuxErrno::Einval);
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(LinuxErrno::Eacces),
            p => parts.push(p),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

fn normalize_ref_prefix(prefix: &str) -> Result<String> {
    if prefix.is_empty() || prefix.starts_with('/') || prefix.ends_with('/') {
        return Err(LinuxErrno::Einval);
    }
    if prefix
        .split('/')
        .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(LinuxErrno::Einval);
    }
    Ok(prefix.to_owned())
}

fn path_suffix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if path == prefix {
        Some("")
    } else {
        path.strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('/'))
    }
}

fn ref_suffix<'a>(ref_name: &'a str, prefix: &str) -> Option<&'a str> {
    if ref_name == prefix {
        Some("")
    } else {
        ref_name
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('/'))
    }
}

trait Tap: Sized {
    fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
        f(&mut self);
        self
    }
}

impl<T> Tap for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use dezh_identity::{Principal, PrincipalKind};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "dezh-linux-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        path
    }

    fn grant(scope: &str, authority: Authority) -> AuthorityGrant {
        let service = Principal::new(PrincipalKind::Service, "linux-personality").unwrap();
        AuthorityGrant::root(service, Scope::new(scope).unwrap(), authority).unwrap()
    }

    fn personality(authority: Authority) -> (PathBuf, LinuxPersonality) {
        let root = temp_store("personality");
        let mut cairn = CairnStore::open(&root).unwrap();
        let object = cairn.put(b"hello legacy").unwrap();
        cairn
            .begin_tx()
            .tap(|tx| tx.set_ref("refs/legacy/home/readme.txt", object))
            .commit("human:ali", "seed legacy view")
            .unwrap();
        let mut linux = LinuxPersonality::new(cairn);
        linux.mount(
            Mount::new(
                "/home/app",
                "refs/legacy/home",
                grant("refs/legacy/home", authority),
            )
            .unwrap(),
        );
        (root, linux)
    }

    #[test]
    fn legacy_app_reads_only_mounted_authorized_ref() {
        let (root, mut linux) = personality(Authority::READ_REF.union(Authority::READ_OBJECT));

        let fd = linux.open("/home/app/readme.txt", O_RDONLY).unwrap();
        let mut out = [0u8; 32];
        let n = linux.read(fd, &mut out).unwrap();

        assert_eq!(&out[..n], b"hello legacy");
        linux.close(fd).unwrap();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unmounted_path_is_not_visible() {
        let (root, mut linux) = personality(Authority::READ_REF.union(Authority::READ_OBJECT));

        let err = linux.open("/etc/passwd", O_RDONLY).unwrap_err();

        assert_eq!(err, LinuxErrno::Eacces);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn path_escape_is_rejected_before_cairn_lookup() {
        let (root, mut linux) = personality(Authority::READ_REF.union(Authority::READ_OBJECT));

        let err = linux.open("/home/app/../secret.txt", O_RDONLY).unwrap_err();

        assert_eq!(err, LinuxErrno::Eacces);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn read_requires_read_ref_and_read_object_authority() {
        let (root, mut linux) = personality(Authority::READ_REF);

        let err = linux.open("/home/app/readme.txt", O_RDONLY).unwrap_err();

        assert_eq!(err, LinuxErrno::Eacces);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn write_updates_cairn_ref_and_records_invocation() {
        let authority = Authority::READ_REF
            .union(Authority::READ_OBJECT)
            .union(Authority::UPDATE_REF);
        let (root, mut linux) = personality(authority);

        let fd = linux
            .open("/home/app/readme.txt", O_WRONLY | O_TRUNC)
            .unwrap();
        let receipt = linux.write(fd, b"new legacy bytes").unwrap();

        let object = linux
            .cairn()
            .get_ref("refs/legacy/home/readme.txt")
            .unwrap();
        assert_eq!(object, receipt.object);
        assert_eq!(linux.cairn().get(object), Some(&b"new legacy bytes"[..]));
        assert_eq!(linux.invocations().len(), 2);
        assert_eq!(linux.invocations()[1].action, "linux.write");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn write_requires_update_ref_authority() {
        let (root, mut linux) = personality(Authority::READ_REF.union(Authority::READ_OBJECT));

        let err = linux.open("/home/app/readme.txt", O_WRONLY).unwrap_err();

        assert_eq!(err, LinuxErrno::Eacces);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn create_is_allowed_only_inside_mounted_view() {
        let authority = Authority::READ_REF
            .union(Authority::READ_OBJECT)
            .union(Authority::UPDATE_REF);
        let (root, mut linux) = personality(authority);

        let fd = linux
            .open("/home/app/new.txt", O_RDWR | O_CREAT | O_TRUNC)
            .unwrap();
        linux.write(fd, b"created").unwrap();
        let object = linux.cairn().get_ref("refs/legacy/home/new.txt").unwrap();

        assert_eq!(linux.cairn().get(object), Some(&b"created"[..]));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unsupported_syscalls_are_explicitly_enosys() {
        let (root, linux) = personality(Authority::READ_REF.union(Authority::READ_OBJECT));

        let err = linux.unsupported_syscall(57).unwrap_err();

        assert_eq!(err, LinuxErrno::Enosys);
        fs::remove_dir_all(root).ok();
    }
}
