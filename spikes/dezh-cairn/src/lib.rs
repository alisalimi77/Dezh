//! # dezh-cairn — Step 2: persistent content-addressed object store
//!
//! Cairn validates Dezh's second irreversible storage decision: durable state is
//! built from immutable content-addressed objects plus small mutable refs. A
//! write never overwrites an old object. A commit only advances refs to new
//! object IDs, which makes rollback and crash recovery structural properties
//! rather than afterthoughts.
//!
//! This is intentionally v0. It is a single append-only log on local disk. There
//! is no GC, encryption, compression, schema engine, distributed sync, semantic
//! directory graph, or high-performance index. Those belong to later phases.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_FILE: &str = "cairn.log";
const MAGIC: &[u8; 8] = b"CAIRN001";
const TYPE_OBJECT: u8 = 1;
const TYPE_COMMIT: u8 = 2;
const TYPE_ROLLBACK: u8 = 3;

/// Stable ID for immutable object content.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        ObjectId(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", hex(&self.0))
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex(&self.0))
    }
}

/// Stable ID for a committed ref movement.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommitId([u8; 32]);

impl CommitId {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        CommitId(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CommitId({})", hex(&self.0))
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex(&self.0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub principal: String,
    pub reason: String,
    pub timestamp_millis: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: CommitId,
    pub ref_name: String,
    pub object: ObjectId,
    pub previous: Option<ObjectId>,
    pub provenance: Provenance,
}

#[derive(Debug)]
pub enum CairnError {
    Io(io::Error),
    EmptyTransaction,
    MultiRefTransactionUnsupported,
    MissingObject(ObjectId),
    MissingRef(String),
    MissingCommit(CommitId),
    InvalidRefName,
    InvalidMetadata,
    ValueTooLarge,
}

impl fmt::Display for CairnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CairnError::Io(e) => write!(f, "io error: {e}"),
            CairnError::EmptyTransaction => write!(f, "transaction has no ref updates"),
            CairnError::MultiRefTransactionUnsupported => {
                write!(f, "Cairn v0 supports one ref update per transaction")
            }
            CairnError::MissingObject(id) => write!(f, "missing object {id}"),
            CairnError::MissingRef(name) => write!(f, "missing ref {name}"),
            CairnError::MissingCommit(id) => write!(f, "missing commit {id}"),
            CairnError::InvalidRefName => write!(f, "ref name must be non-empty"),
            CairnError::InvalidMetadata => write!(f, "principal and reason must be non-empty"),
            CairnError::ValueTooLarge => write!(f, "value is too large for the v0 log format"),
        }
    }
}

impl std::error::Error for CairnError {}

impl From<io::Error> for CairnError {
    fn from(value: io::Error) -> Self {
        CairnError::Io(value)
    }
}

pub type Result<T> = std::result::Result<T, CairnError>;

pub struct CairnStore {
    root: PathBuf,
    log: File,
    objects: HashMap<ObjectId, Vec<u8>>,
    refs: HashMap<String, ObjectId>,
    commits: HashMap<CommitId, CommitInfo>,
    ref_history: HashMap<String, Vec<CommitId>>,
    next_sequence: u64,
}

impl CairnStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        let log_path = root.join(LOG_FILE);
        let (records, next_sequence) = replay_log(&log_path)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&log_path)?;

        let mut store = CairnStore {
            root,
            log,
            objects: HashMap::new(),
            refs: HashMap::new(),
            commits: HashMap::new(),
            ref_history: HashMap::new(),
            next_sequence,
        };
        for record in records {
            store.apply_replayed(record);
        }
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put(&mut self, bytes: impl AsRef<[u8]>) -> Result<ObjectId> {
        let bytes = bytes.as_ref();
        let id = object_id(bytes);
        if !self.objects.contains_key(&id) {
            let record = encode_object(id, bytes)?;
            self.append_record(&record)?;
            self.objects.insert(id, bytes.to_vec());
        }
        Ok(id)
    }

    pub fn get(&self, id: ObjectId) -> Option<&[u8]> {
        self.objects.get(&id).map(Vec::as_slice)
    }

    pub fn begin_tx(&mut self) -> Tx<'_> {
        Tx {
            store: self,
            puts: Vec::new(),
            ref_updates: Vec::new(),
        }
    }

    pub fn get_ref(&self, name: &str) -> Option<ObjectId> {
        self.refs.get(name).copied()
    }

    pub fn history(&self, name: &str) -> Vec<CommitId> {
        self.ref_history.get(name).cloned().unwrap_or_default()
    }

    pub fn commit_info(&self, id: CommitId) -> Option<&CommitInfo> {
        self.commits.get(&id)
    }

    pub fn rollback(
        &mut self,
        name: &str,
        target: CommitId,
        principal: &str,
        reason: &str,
    ) -> Result<CommitId> {
        validate_ref_name(name)?;
        validate_metadata(principal, reason)?;
        let target_info = self
            .commits
            .get(&target)
            .ok_or(CairnError::MissingCommit(target))?;
        if target_info.ref_name != name {
            return Err(CairnError::MissingCommit(target));
        }
        let object = target_info.object;
        let previous = self.refs.get(name).copied();
        let sequence = self.take_sequence();
        let provenance = Provenance {
            principal: principal.to_owned(),
            reason: reason.to_owned(),
            timestamp_millis: now_millis(),
        };
        let id = commit_id(sequence, name, object, previous, &provenance);
        let record = encode_ref_move(
            TYPE_ROLLBACK,
            id,
            sequence,
            name,
            object,
            previous,
            &provenance,
        )?;
        self.append_record(&record)?;
        self.apply_commit(CommitInfo {
            id,
            ref_name: name.to_owned(),
            object,
            previous,
            provenance,
        });
        Ok(id)
    }

    fn commit_updates(
        &mut self,
        mut ref_updates: Vec<(String, ObjectId)>,
        principal: &str,
        reason: &str,
    ) -> Result<CommitId> {
        if ref_updates.is_empty() {
            return Err(CairnError::EmptyTransaction);
        }
        if ref_updates.len() > 1 {
            return Err(CairnError::MultiRefTransactionUnsupported);
        }
        validate_metadata(principal, reason)?;
        for (name, id) in &ref_updates {
            validate_ref_name(name)?;
            if !self.objects.contains_key(id) {
                return Err(CairnError::MissingObject(*id));
            }
        }

        let (name, object) = ref_updates
            .pop()
            .expect("non-empty ref_updates checked above");
        let previous = self.refs.get(&name).copied();
        let sequence = self.take_sequence();
        let provenance = Provenance {
            principal: principal.to_owned(),
            reason: reason.to_owned(),
            timestamp_millis: now_millis(),
        };
        let id = commit_id(sequence, &name, object, previous, &provenance);
        let record = encode_ref_move(
            TYPE_COMMIT,
            id,
            sequence,
            &name,
            object,
            previous,
            &provenance,
        )?;
        self.append_record(&record)?;
        self.apply_commit(CommitInfo {
            id,
            ref_name: name,
            object,
            previous,
            provenance,
        });
        Ok(id)
    }

    fn take_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        sequence
    }

    fn append_record(&mut self, payload: &[u8]) -> Result<()> {
        let len = u64::try_from(payload.len()).map_err(|_| CairnError::ValueTooLarge)?;
        let checksum = blake3::hash(payload);
        self.log.write_all(MAGIC)?;
        self.log.write_all(&len.to_le_bytes())?;
        self.log.write_all(checksum.as_bytes())?;
        self.log.write_all(payload)?;
        self.log.sync_data()?;
        Ok(())
    }

    fn apply_replayed(&mut self, record: LogRecord) {
        match record {
            LogRecord::Object { id, bytes } => {
                self.objects.entry(id).or_insert(bytes);
            }
            LogRecord::Commit { info, sequence } | LogRecord::Rollback { info, sequence } => {
                self.next_sequence = self.next_sequence.max(sequence + 1);
                self.apply_commit(info);
            }
        }
    }

    fn apply_commit(&mut self, info: CommitInfo) {
        self.refs.insert(info.ref_name.clone(), info.object);
        self.ref_history
            .entry(info.ref_name.clone())
            .or_default()
            .push(info.id);
        self.commits.insert(info.id, info);
    }
}

pub struct Tx<'a> {
    store: &'a mut CairnStore,
    puts: Vec<(ObjectId, Vec<u8>)>,
    ref_updates: Vec<(String, ObjectId)>,
}

impl<'a> Tx<'a> {
    pub fn put(&mut self, bytes: impl AsRef<[u8]>) -> ObjectId {
        let bytes = bytes.as_ref();
        let id = object_id(bytes);
        if !self.store.objects.contains_key(&id)
            && !self.puts.iter().any(|(existing, _)| *existing == id)
        {
            self.puts.push((id, bytes.to_vec()));
        }
        id
    }

    pub fn set_ref(&mut self, name: impl Into<String>, id: ObjectId) {
        self.ref_updates.push((name.into(), id));
    }

    pub fn commit(mut self, principal: &str, reason: &str) -> Result<CommitId> {
        for (id, bytes) in self.puts.drain(..) {
            if !self.store.objects.contains_key(&id) {
                let record = encode_object(id, &bytes)?;
                self.store.append_record(&record)?;
                self.store.objects.insert(id, bytes);
            }
        }
        self.store
            .commit_updates(self.ref_updates, principal, reason)
    }
}

enum LogRecord {
    Object { id: ObjectId, bytes: Vec<u8> },
    Commit { info: CommitInfo, sequence: u64 },
    Rollback { info: CommitInfo, sequence: u64 },
}

fn replay_log(path: &Path) -> Result<(Vec<LogRecord>, u64)> {
    if !path.exists() {
        return Ok((Vec::new(), 0));
    }

    let mut file = File::open(path)?;
    let mut records = Vec::new();
    let mut next_sequence = 0u64;
    loop {
        let mut magic = [0u8; 8];
        match file.read_exact(&mut magic) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if &magic != MAGIC {
            break;
        }

        let mut len_buf = [0u8; 8];
        let mut checksum = [0u8; 32];
        if file.read_exact(&mut len_buf).is_err() || file.read_exact(&mut checksum).is_err() {
            break;
        }
        let len = u64::from_le_bytes(len_buf);
        let Ok(len) = usize::try_from(len) else {
            break;
        };
        let mut payload = vec![0u8; len];
        if file.read_exact(&mut payload).is_err() {
            break;
        }
        if blake3::hash(&payload).as_bytes() != &checksum {
            break;
        }
        let Some(record) = decode_record(&payload) else {
            break;
        };
        match &record {
            LogRecord::Commit { sequence, .. } | LogRecord::Rollback { sequence, .. } => {
                next_sequence = next_sequence.max(sequence + 1);
            }
            LogRecord::Object { .. } => {}
        }
        records.push(record);
    }
    Ok((records, next_sequence))
}

fn encode_object(id: ObjectId, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.push(TYPE_OBJECT);
    out.extend_from_slice(id.as_bytes());
    write_bytes(&mut out, bytes)?;
    Ok(out)
}

fn encode_ref_move(
    ty: u8,
    commit: CommitId,
    sequence: u64,
    name: &str,
    object: ObjectId,
    previous: Option<ObjectId>,
    provenance: &Provenance,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.push(ty);
    out.extend_from_slice(commit.as_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    write_string(&mut out, name)?;
    out.extend_from_slice(object.as_bytes());
    match previous {
        Some(id) => {
            out.push(1);
            out.extend_from_slice(id.as_bytes());
        }
        None => out.push(0),
    }
    write_string(&mut out, &provenance.principal)?;
    write_string(&mut out, &provenance.reason)?;
    out.extend_from_slice(&provenance.timestamp_millis.to_le_bytes());
    Ok(out)
}

fn decode_record(payload: &[u8]) -> Option<LogRecord> {
    let mut c = Cursor::new(payload);
    let ty = c.u8()?;
    match ty {
        TYPE_OBJECT => {
            let id = ObjectId(c.array_32()?);
            let bytes = c.bytes()?;
            Some(LogRecord::Object { id, bytes })
        }
        TYPE_COMMIT | TYPE_ROLLBACK => {
            let id = CommitId(c.array_32()?);
            let sequence = c.u64()?;
            let ref_name = c.string()?;
            let object = ObjectId(c.array_32()?);
            let previous = match c.u8()? {
                0 => None,
                1 => Some(ObjectId(c.array_32()?)),
                _ => return None,
            };
            let principal = c.string()?;
            let reason = c.string()?;
            let timestamp_millis = c.u128()?;
            if !c.is_done() {
                return None;
            }
            let info = CommitInfo {
                id,
                ref_name,
                object,
                previous,
                provenance: Provenance {
                    principal,
                    reason,
                    timestamp_millis,
                },
            };
            if ty == TYPE_COMMIT {
                Some(LogRecord::Commit { info, sequence })
            } else {
                Some(LogRecord::Rollback { info, sequence })
            }
        }
        _ => None,
    }
}

fn object_id(bytes: &[u8]) -> ObjectId {
    ObjectId(*blake3::hash(bytes).as_bytes())
}

fn commit_id(
    sequence: u64,
    name: &str,
    object: ObjectId,
    previous: Option<ObjectId>,
    provenance: &Provenance,
) -> CommitId {
    let mut h = blake3::Hasher::new();
    h.update(&sequence.to_le_bytes());
    h.update(name.as_bytes());
    h.update(object.as_bytes());
    if let Some(previous) = previous {
        h.update(previous.as_bytes());
    }
    h.update(provenance.principal.as_bytes());
    h.update(provenance.reason.as_bytes());
    h.update(&provenance.timestamp_millis.to_le_bytes());
    CommitId(*h.finalize().as_bytes())
}

fn validate_ref_name(name: &str) -> Result<()> {
    if name.is_empty() {
        Err(CairnError::InvalidRefName)
    } else {
        Ok(())
    }
}

fn validate_metadata(principal: &str, reason: &str) -> Result<()> {
    if principal.is_empty() || reason.is_empty() {
        Err(CairnError::InvalidMetadata)
    } else {
        Ok(())
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn write_string(out: &mut Vec<u8>, value: &str) -> Result<()> {
    write_bytes(out, value.as_bytes())
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    let len = u32::try_from(value.len()).map_err(|_| CairnError::ValueTooLarge)?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
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

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    fn is_done(&self) -> bool {
        self.pos == self.bytes.len()
    }

    fn u8(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array_4()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.array_8()?))
    }

    fn u128(&mut self) -> Option<u128> {
        Some(u128::from_le_bytes(self.array_16()?))
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let len = self.u32()? as usize;
        let end = self.pos.checked_add(len)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice.to_vec())
    }

    fn string(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?).ok()
    }

    fn array_4(&mut self) -> Option<[u8; 4]> {
        let end = self.pos.checked_add(4)?;
        let arr = self.bytes.get(self.pos..end)?.try_into().ok()?;
        self.pos = end;
        Some(arr)
    }

    fn array_8(&mut self) -> Option<[u8; 8]> {
        let end = self.pos.checked_add(8)?;
        let arr = self.bytes.get(self.pos..end)?.try_into().ok()?;
        self.pos = end;
        Some(arr)
    }

    fn array_16(&mut self) -> Option<[u8; 16]> {
        let end = self.pos.checked_add(16)?;
        let arr = self.bytes.get(self.pos..end)?.try_into().ok()?;
        self.pos = end;
        Some(arr)
    }

    fn array_32(&mut self) -> Option<[u8; 32]> {
        let end = self.pos.checked_add(32)?;
        let arr = self.bytes.get(self.pos..end)?.try_into().ok()?;
        self.pos = end;
        Some(arr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;

    fn temp_store(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "dezh-cairn-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        path
    }

    #[test]
    fn duplicate_content_has_same_object_id() {
        let root = temp_store("dedup");
        let mut store = CairnStore::open(&root).unwrap();

        let a = store.put(b"same").unwrap();
        let b = store.put(b"same").unwrap();

        assert_eq!(a, b);
        assert_eq!(store.get(a), Some(&b"same"[..]));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn old_objects_survive_ref_updates() {
        let root = temp_store("old-objects");
        let mut store = CairnStore::open(&root).unwrap();

        let first = store.put(b"v1").unwrap();
        store
            .begin_tx()
            .tap(|tx| tx.set_ref("doc", first))
            .commit("human:ali", "initial")
            .unwrap();
        let second = store.put(b"v2").unwrap();
        store
            .begin_tx()
            .tap(|tx| tx.set_ref("doc", second))
            .commit("agent:writer", "update")
            .unwrap();

        assert_eq!(store.get(first), Some(&b"v1"[..]));
        assert_eq!(store.get(second), Some(&b"v2"[..]));
        assert_eq!(store.get_ref("doc"), Some(second));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn transaction_put_commit_and_reopen_replays_state() {
        let root = temp_store("reopen");
        let commit;
        let object;
        {
            let mut store = CairnStore::open(&root).unwrap();
            let mut tx = store.begin_tx();
            object = tx.put(b"durable");
            tx.set_ref("agent/work", object);
            commit = tx.commit("agent:coder", "write file").unwrap();
        }

        let store = CairnStore::open(&root).unwrap();
        assert_eq!(store.get(object), Some(&b"durable"[..]));
        assert_eq!(store.get_ref("agent/work"), Some(object));
        assert_eq!(store.history("agent/work"), vec![commit]);
        let info = store.commit_info(commit).unwrap();
        assert_eq!(info.provenance.principal, "agent:coder");
        assert_eq!(info.provenance.reason, "write file");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rollback_moves_ref_back_and_records_provenance() {
        let root = temp_store("rollback");
        let mut store = CairnStore::open(&root).unwrap();
        let first = store.put(b"good").unwrap();
        let c1 = store
            .begin_tx()
            .tap(|tx| tx.set_ref("doc", first))
            .commit("human:ali", "known good")
            .unwrap();
        let second = store.put(b"bad").unwrap();
        store
            .begin_tx()
            .tap(|tx| tx.set_ref("doc", second))
            .commit("agent:buggy", "bad edit")
            .unwrap();

        let rollback = store
            .rollback("doc", c1, "human:ali", "undo agent edit")
            .unwrap();

        assert_eq!(store.get_ref("doc"), Some(first));
        assert_eq!(store.history("doc").len(), 3);
        let info = store.commit_info(rollback).unwrap();
        assert_eq!(info.object, first);
        assert_eq!(info.previous, Some(second));
        assert_eq!(info.provenance.reason, "undo agent edit");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn v0_rejects_multi_ref_transactions() {
        let root = temp_store("multi-ref");
        let mut store = CairnStore::open(&root).unwrap();
        let first = store.put(b"one").unwrap();
        let second = store.put(b"two").unwrap();
        let mut tx = store.begin_tx();
        tx.set_ref("a", first);
        tx.set_ref("b", second);

        let err = tx.commit("human:ali", "multi ref").unwrap_err();

        assert!(matches!(err, CairnError::MultiRefTransactionUnsupported));
        assert_eq!(store.get_ref("a"), None);
        assert_eq!(store.get_ref("b"), None);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn truncated_tail_is_ignored_on_replay() {
        let root = temp_store("truncated");
        let first;
        {
            let mut store = CairnStore::open(&root).unwrap();
            first = store.put(b"stable").unwrap();
            store
                .begin_tx()
                .tap(|tx| tx.set_ref("doc", first))
                .commit("human:ali", "stable")
                .unwrap();
            let mut log = OpenOptions::new()
                .append(true)
                .open(root.join(LOG_FILE))
                .unwrap();
            log.write_all(MAGIC).unwrap();
            log.write_all(&100u64.to_le_bytes()).unwrap();
            log.write_all(&[7u8; 12]).unwrap();
            log.flush().unwrap();
            log.rewind().ok();
        }

        let store = CairnStore::open(&root).unwrap();
        assert_eq!(store.get_ref("doc"), Some(first));
        assert_eq!(store.get(first), Some(&b"stable"[..]));
        assert_eq!(store.history("doc").len(), 1);
        fs::remove_dir_all(root).ok();
    }

    trait Tap: Sized {
        fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
            f(&mut self);
            self
        }
    }

    impl<T> Tap for T {}
}
