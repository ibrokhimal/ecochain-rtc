//! Encrypted workspace journal and opaque per-instance artifacts.
//! All methods are blocking and must run on a persistence worker, never the SFU actor.
mod stream;

use anyhow::{bail, ensure, Context, Result};
use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};
use zeroize::Zeroizing;

const MAX_RECORD: usize = 24 * 1024 * 1024;
const MAX_BLOB: usize = 64 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"ECOVLT01";
const WORKER_MARKER: &str = ".ecochain-worker";
const WORKER_MARKER_BYTES: &[u8] = b"ecochain-private-worker-v1";
type MemberCache = HashMap<String, (Vec<(u64, SystemTime)>, BTreeSet<String>)>;

/// A durable meeting occurrence, including admitted identities for archive access.
#[derive(Clone, Serialize, Deserialize)]
pub struct Archive {
    /// Canonical workspace snapshot.
    pub state: Value,
    /// Admitted user identities, retained after disconnect.
    pub members: BTreeSet<String>,
    /// Last update as Unix seconds.
    pub updated_at: u64,
}

/// Private storage root with envelope encryption and serialized journal writes.
pub struct Vault {
    root: PathBuf,
    master: Zeroizing<[u8; 32]>,
    retention_secs: u64,
    lock: Mutex<()>,
    members_cache: Mutex<MemberCache>,
}
impl Vault {
    /// Opens a configured root. The caller supplies a 32-byte master key from env/KMS.
    pub fn open(root: impl AsRef<Path>, key: [u8; 32], retention_secs: u64) -> Result<Arc<Self>> {
        ensure!(retention_secs > 0, "retention must be positive");
        private_dir(root.as_ref())?;
        Ok(Arc::new(Self {
            root: root.as_ref().to_owned(),
            master: Zeroizing::new(key),
            retention_secs,
            lock: Mutex::new(()),
            members_cache: Mutex::new(Default::default()),
        }))
    }
    /// Fsyncs one encrypted state record. A torn final record is removed before append.
    pub fn append(&self, archive: &Archive) -> Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        self.append_locked(archive)
    }
    fn append_locked(&self, archive: &Archive) -> Result<()> {
        let instance = archive.state["instance_id"]
            .as_str()
            .context("missing instance id")?;
        let dir = self.directory(instance)?;
        private_dir(&dir)?;
        let (old, valid, count) = self.load(instance)?;
        if let Some(old) = old {
            ensure!(
                archive.state["revision"].as_u64() >= old.state["revision"].as_u64(),
                "revision regression"
            );
            if old.state == archive.state && old.members == archive.members {
                return Ok(());
            }
        }
        let plaintext = serde_json::to_vec(archive)?;
        ensure!(plaintext.len() <= MAX_RECORD - 128, "snapshot too large");
        let encrypted = self.encrypt(&plaintext, format!("{instance}/workspace").as_bytes())?;
        let journal = dir.join("workspace.journal");
        let mut file = private_file(&journal, false)?;
        file.set_len(valid)?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::End(0))?;
        file.write_all(&(encrypted.len() as u32).to_be_bytes())?;
        file.write_all(&encrypted)?;
        file.sync_all()?;
        if count >= 31 {
            atomic_write(&dir.join("workspace.snapshot"), &encrypted)?;
            file.set_len(0)?;
            file.sync_all()?;
        }
        File::open(&dir)?.sync_all()?;
        Ok(())
    }
    /// Applies an offline archive update under the same journal lock as append.
    pub fn update_archive(
        &self,
        instance: &str,
        update: impl FnOnce(&mut Archive) -> Result<()>,
    ) -> Result<Archive> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let mut archive = self.load(instance)?.0.context("archive not found")?;
        update(&mut archive)?;
        archive.updated_at = crate::auth::unix_now();
        self.append_locked(&archive)?;
        Ok(archive)
    }
    /// Checks membership without cloning/replaying the whiteboard on every media range.
    pub fn is_member(&self, instance: &str, identity: &str) -> Result<bool> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let dir = self.directory(instance)?;
        let mut fingerprint = Vec::new();
        for file in ["workspace.snapshot", "workspace.journal"] {
            match fs::metadata(dir.join(file)) {
                Ok(metadata) => fingerprint.push((metadata.len(), metadata.modified()?)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    fingerprint.push((0, std::time::UNIX_EPOCH))
                }
                Err(e) => return Err(e.into()),
            }
        }
        if let Some((cached, members)) = self.members_cache.lock().unwrap().get(instance) {
            if *cached == fingerprint {
                return Ok(members.contains(identity));
            }
        }
        let archive = self
            .load(instance)?
            .0
            .context("meeting archive not found")?;
        let allowed = archive.members.contains(identity);
        let mut cache = self.members_cache.lock().unwrap();
        if cache.len() >= 1024 {
            cache.clear();
        }
        cache.insert(instance.to_owned(), (fingerprint, archive.members));
        Ok(allowed)
    }

    /// Replays a snapshot and complete journal records; authentication failure is fatal.
    pub fn read(&self, instance: &str) -> Result<Archive> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        self.load(instance)?.0.context("meeting archive not found")
    }
    fn load(&self, instance: &str) -> Result<(Option<Archive>, u64, usize)> {
        let dir = self.directory(instance)?;
        let aad = format!("{instance}/workspace");
        let mut latest = match fs::read(dir.join("workspace.snapshot")) {
            Ok(bytes) => Some(serde_json::from_slice::<Archive>(
                &self.decrypt(&bytes, aad.as_bytes())?,
            )?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut file = match File::open(dir.join("workspace.journal")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((latest, 0, 0)),
            Err(e) => return Err(e.into()),
        };
        let mut valid = 0;
        let mut count = 0;
        loop {
            let mut length = [0; 4];
            match file.read_exact(&mut length) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            let length = u32::from_be_bytes(length) as usize;
            ensure!(
                (128..=MAX_RECORD).contains(&length),
                "invalid encrypted journal record"
            );
            let mut bytes = vec![0; length];
            match file.read_exact(&mut bytes) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            let candidate: Archive =
                serde_json::from_slice(&self.decrypt(&bytes, aad.as_bytes())?)?;
            ensure!(
                candidate.state["instance_id"] == instance,
                "foreign workspace record"
            );
            if latest.as_ref().is_none_or(|old| {
                candidate.state["revision"].as_u64() >= old.state["revision"].as_u64()
            }) {
                latest = Some(candidate);
            }
            valid += 4 + length as u64;
            count += 1;
        }
        Ok((latest, valid, count))
    }
    /// Stores an opaque encrypted artifact under a random, instance-bound identifier.
    pub fn put(&self, instance: &str, bytes: &[u8]) -> Result<String> {
        ensure!(bytes.len() <= MAX_BLOB, "artifact too large");
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let dir = self.directory(instance)?;
        private_dir(&dir)?;
        let id = uuid::Uuid::new_v4().to_string();
        atomic_write(
            &dir.join(format!("{id}.blob")),
            &self.encrypt(bytes, format!("{instance}/{id}").as_bytes())?,
        )?;
        Ok(id)
    }
    /// Loads an artifact. HTTP callers must validate archive membership first.
    pub fn get(&self, instance: &str, id: &str) -> Result<Vec<u8>> {
        validate_id(id)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let file = self.directory(instance)?.join(format!("{id}.blob"));
        ensure!(
            fs::metadata(&file)?.len() <= MAX_BLOB as u64 + 128,
            "artifact too large"
        );
        self.decrypt(&fs::read(file)?, format!("{instance}/{id}").as_bytes())
    }
    /// Removes expired occurrences and every associated artifact together.
    pub fn purge(&self, now: u64) -> Result<usize> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let mut removed = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_dir() || validate_id(&id).is_err() {
                continue;
            }
            if let Some(archive) = self.load(&id)?.0 {
                if now.saturating_sub(archive.updated_at) > self.retention_secs {
                    fs::remove_dir_all(entry.path())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
    fn directory(&self, instance: &str) -> Result<PathBuf> {
        validate_id(instance)?;
        Ok(self.root.join(instance))
    }
    fn encrypt(&self, bytes: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        let mut key = Zeroizing::new([0; 32]);
        OsRng.fill_bytes(key.as_mut());
        let wrapped = seal(self.master.as_ref(), key.as_ref(), aad)?;
        let encrypted = seal(key.as_ref(), bytes, aad)?;
        let mut result = Vec::with_capacity(8 + wrapped.len() + encrypted.len());
        result.extend_from_slice(MAGIC);
        result.extend(wrapped);
        result.extend(encrypted);
        Ok(result)
    }
    fn decrypt(&self, bytes: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            bytes.len() >= 96 && &bytes[..8] == MAGIC,
            "invalid encrypted artifact"
        );
        let key = Zeroizing::new(open(self.master.as_ref(), &bytes[8..68], aad)?);
        open(key.as_ref(), &bytes[68..], aad)
    }
}
/// Creates an owner-only worker directory and durably marks it before any plaintext is written.
pub(crate) fn create_private_worker_dir(root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "invalid worker spool root"
    );
    let path = root.join(uuid::Uuid::new_v4().to_string());
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path)?;
    let result = atomic_write(&path.join(WORKER_MARKER), WORKER_MARKER_BYTES)
        .and_then(|()| File::open(root)?.sync_all().map_err(Into::into));
    if result.is_err() {
        let _ = fs::remove_dir_all(&path);
    }
    result.map(|()| path)
}

/// Removes private, exactly marked worker spools from an earlier run.
/// Call at startup, before any workers can create a directory in this root.
pub fn remove_abandoned_worker_dirs(root: &Path) -> Result<usize> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if !metadata.is_dir() => return Ok(0),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    }
    let mut removed = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || validate_id(&entry.file_name().to_string_lossy()).is_err()
        {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if entry.metadata()?.permissions().mode() & 0o777 != 0o700 {
                continue;
            }
        }
        let path = entry.path();
        let marker = path.join(WORKER_MARKER);
        let marker_meta = match fs::symlink_metadata(&marker) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if !marker_meta.is_file() || marker_meta.len() != WORKER_MARKER_BYTES.len() as u64 {
            continue;
        }
        let mut bytes = Vec::new();
        File::open(marker)?
            .take(WORKER_MARKER_BYTES.len() as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes != WORKER_MARKER_BYTES {
            continue;
        }
        fs::remove_dir_all(path)?;
        removed += 1;
    }
    Ok(removed)
}
fn validate_id(id: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(id).is_ok() && id.len() == 36,
        "invalid artifact identifier"
    );
    Ok(())
}
fn seal(key: &[u8], data: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, key)
            .map_err(|_| anyhow::anyhow!("invalid encryption key"))?,
    );
    let mut nonce = [0; 12];
    OsRng.fill_bytes(&mut nonce);
    let mut bytes = data.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(aad),
        &mut bytes,
    )
    .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    let mut result = nonce.to_vec();
    result.extend(bytes);
    Ok(result)
}
fn open(key: &[u8], data: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 28 {
        bail!("truncated encrypted artifact");
    }
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, key)
            .map_err(|_| anyhow::anyhow!("invalid encryption key"))?,
    );
    let nonce: [u8; 12] = data[..12].try_into()?;
    let mut bytes = data[12..].to_vec();
    let plaintext = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            &mut bytes,
        )
        .map_err(|_| anyhow::anyhow!("artifact authentication failed"))?;
    Ok(plaintext.to_vec())
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn private_file(path: &Path, exclusive: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = private_file(&temp, true)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
