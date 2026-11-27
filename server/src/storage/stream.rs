//! Seekable chunk encryption for long recordings; no whole-file plaintext buffer.
use super::{private_dir, private_file, validate_id, Vault};
use anyhow::{ensure, Context, Result};
use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use rand::{rngs::OsRng, RngCore};
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Seek, SeekFrom, Write},
    path::Path,
};
use zeroize::Zeroizing;
const CHUNK: usize = 65536;
const HEADER: usize = 81;
const MAGIC: &[u8; 8] = b"ECOSTR01";
const MAX_JOB_BYTES: usize = 4 * 1024 * 1024;
const MAX_JOB_FILE_BYTES: u64 = MAX_JOB_BYTES as u64 + 128;
const MAX_JOB_BYTES_PER_INSTANCE: u64 = 16 * 1024 * 1024;
const MAX_JOBS_PER_INSTANCE: usize = 256;
// Reserve room for small in-flight jobs to save a terminal status at the quota.
const MIN_JOB_CHARGE_BYTES: u64 = 1024;
struct StreamHeader {
    len: u64,
    mime: &'static str,
    key: Zeroizing<Vec<u8>>,
    prefix: [u8; 4],
    aad: Vec<u8>,
}
impl Vault {
    /// Encrypts a finalized worker output in independently authenticated 64 KiB chunks.
    pub fn put_file(&self, instance: &str, source: &Path, mime: &str) -> Result<String> {
        let code = match mime {
            "video/mp4" => 1,
            "audio/mp4" => 2,
            "application/x-ndjson" => 3,
            "application/json" => 4,
            _ => anyhow::bail!("unsupported stream MIME"),
        };
        let mut input = File::open(source)?;
        let len = input.metadata()?.len();
        ensure!(
            len <= 16 * 1024 * 1024 * 1024,
            "recording artifact exceeds 16 GiB"
        );
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let dir = self.directory(instance)?;
        private_dir(&dir)?;
        let id = uuid::Uuid::new_v4().to_string();
        let path = dir.join(format!("{id}.stream"));
        let temp = dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut key = Zeroizing::new([0u8; 32]);
            OsRng.fill_bytes(key.as_mut());
            let mut prefix = [0; 4];
            OsRng.fill_bytes(&mut prefix);
            let aad = aad(instance, &id, len, code, prefix);
            let wrapped = super::seal(self.master.as_ref(), key.as_ref(), &aad)?;
            let cipher = LessSafeKey::new(
                UnboundKey::new(&AES_256_GCM, key.as_ref())
                    .map_err(|_| anyhow::anyhow!("invalid recording key"))?,
            );
            let mut output = private_file(&temp, true)?;
            output.write_all(MAGIC)?;
            output.write_all(&len.to_be_bytes())?;
            output.write_all(&[code])?;
            output.write_all(&prefix)?;
            output.write_all(&wrapped)?;
            let mut remaining = len;
            let mut index = 0u64;
            while remaining > 0 {
                let take = remaining.min(CHUNK as u64) as usize;
                let mut bytes = Zeroizing::new(vec![0; take]);
                input.read_exact(&mut bytes)?;
                let mut chunk_aad = aad.clone();
                chunk_aad.extend_from_slice(&index.to_be_bytes());
                cipher
                    .seal_in_place_append_tag(
                        nonce(prefix, index),
                        Aad::from(&chunk_aad),
                        &mut *bytes,
                    )
                    .map_err(|_| anyhow::anyhow!("recording encryption failed"))?;
                output.write_all(&bytes)?;
                remaining -= take as u64;
                index += 1;
            }
            output.sync_all()?;
            fs::rename(&temp, &path)?;
            File::open(&dir)?.sync_all()?;
            Ok(id)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
    /// Authenticates the header before trusting stream size or content type.
    pub fn stream_metadata(&self, instance: &str, id: &str) -> Result<(u64, &'static str)> {
        let mut file = self.stream_file(instance, id)?;
        let header = self.stream_header(instance, id, &mut file)?;
        Ok((header.len, header.mime))
    }
    /// Decrypts a bounded byte range, authenticating every touched chunk.
    pub fn read_range(
        &self,
        instance: &str,
        id: &str,
        start: u64,
        length: usize,
    ) -> Result<Vec<u8>> {
        ensure!(length <= 8 * 1024 * 1024, "range too large");
        let mut file = self.stream_file(instance, id)?;
        let header = self.stream_header(instance, id, &mut file)?;
        ensure!(
            start <= header.len && (length as u64) <= header.len - start,
            "range outside artifact"
        );
        let cipher = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &header.key)
                .map_err(|_| anyhow::anyhow!("invalid recording key"))?,
        );
        let mut result = Vec::with_capacity(length);
        let mut offset = start;
        while result.len() < length {
            let index = offset / CHUNK as u64;
            let chunk_start = index * CHUNK as u64;
            let plain_len = (header.len - chunk_start).min(CHUNK as u64) as usize;
            file.seek(SeekFrom::Start(HEADER as u64 + index * (CHUNK + 16) as u64))?;
            let mut bytes = Zeroizing::new(vec![0; plain_len + 16]);
            file.read_exact(&mut bytes)?;
            let mut chunk_aad = header.aad.clone();
            chunk_aad.extend_from_slice(&index.to_be_bytes());
            let plaintext = cipher
                .open_in_place(
                    nonce(header.prefix, index),
                    Aad::from(&chunk_aad),
                    &mut bytes,
                )
                .map_err(|_| anyhow::anyhow!("recording chunk authentication failed"))?;
            let within = (offset - chunk_start) as usize;
            let take = (length - result.len()).min(plain_len - within);
            result.extend_from_slice(&plaintext[within..within + take]);
            offset += take as u64;
        }
        Ok(result)
    }
    fn stream_file(&self, instance: &str, id: &str) -> Result<File> {
        validate_id(id)?;
        Ok(File::open(
            self.directory(instance)?.join(format!("{id}.stream")),
        )?)
    }
    fn stream_header(&self, instance: &str, id: &str, file: &mut File) -> Result<StreamHeader> {
        let mut bytes = [0; HEADER];
        file.read_exact(&mut bytes)?;
        ensure!(&bytes[..8] == MAGIC, "invalid recording header");
        let len = u64::from_be_bytes(bytes[8..16].try_into()?);
        ensure!(
            len <= 16 * 1024 * 1024 * 1024,
            "recording length out of bounds"
        );
        let code = bytes[16];
        let mime = match code {
            1 => "video/mp4",
            2 => "audio/mp4",
            3 => "application/x-ndjson",
            4 => "application/json",
            _ => anyhow::bail!("invalid recording MIME"),
        };
        let prefix: [u8; 4] = bytes[17..21].try_into()?;
        let aad = aad(instance, id, len, code, prefix);
        let key = Zeroizing::new(super::open(self.master.as_ref(), &bytes[21..], &aad)?);
        ensure!(key.len() == 32, "invalid recording key");
        let expected = HEADER as u64 + len + len.div_ceil(CHUNK as u64) * 16;
        ensure!(
            file.metadata()?.len() == expected,
            "truncated recording artifact"
        );
        Ok(StreamHeader {
            len,
            mime,
            key,
            prefix,
            aad,
        })
    }
    /// Atomically stores encrypted recording/provider job metadata.
    pub fn write_job(&self, instance: &str, id: &str, value: &Value) -> Result<()> {
        validate_id(id)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let dir = self.directory(instance)?;
        private_dir(&dir)?;
        let path = dir.join(format!("{id}.job"));
        let previous = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(metadata.is_file(), "invalid job file");
                metadata.len()
            }
            Err(error) if error.kind() == ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        let encrypted = self.encode_job(instance, id, &serde_json::to_vec(value)?)?;
        let encrypted_len = encrypted.len() as u64;
        if previous == 0
            || encrypted_len.max(MIN_JOB_CHARGE_BYTES) > previous.max(MIN_JOB_CHARGE_BYTES)
        {
            let mut budget = JobBudget::new();
            budget.add(encrypted_len)?;
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                if job_file_id(&entry)?.is_some_and(|other| other != id) {
                    budget.add(entry.metadata()?.len())?;
                }
            }
        }
        super::atomic_write(&path, &encrypted)
    }
    /// Reads one authenticated job without loading unrelated transcripts.
    pub fn read_job(&self, instance: &str, id: &str) -> Result<Value> {
        validate_id(id)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let path = self.directory(instance)?.join(format!("{id}.job"));
        self.decode_job(instance, id, &read_job_file(&path)?)
    }
    /// Lists authenticated job metadata for an already authorized instance.
    pub fn jobs(&self, instance: &str) -> Result<Vec<Value>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let dir = self.directory(instance)?;
        let mut result = Vec::new();
        let mut budget = JobBudget::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let Some(id) = job_file_id(&entry)? else {
                continue;
            };
            budget.add(entry.metadata()?.len())?;
            let encrypted = read_job_file(&entry.path())?;
            result.push(self.decode_job(instance, &id, &encrypted)?);
        }
        Ok(result)
    }

    /// Marks interrupted jobs terminal before the server accepts work.
    /// An interrupted Jira submission remains reconciliation-only across restarts.
    pub fn recover_interrupted_jobs(&self) -> Result<usize> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock failed"))?;
        let mut recovered = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let instance = name.to_string_lossy();
            if !entry.file_type()?.is_dir() || validate_id(&instance).is_err() {
                continue;
            }
            for entry in fs::read_dir(entry.path())? {
                let entry = entry?;
                let Some(id) = job_file_id(&entry)? else {
                    continue;
                };
                let path = entry.path();
                let mut job = self.decode_job(&instance, &id, &read_job_file(&path)?)?;
                let (status, error) = match (job["kind"].as_str(), job["status"].as_str()) {
                    (Some("transcript" | "summary"), Some("queued" | "running"))
                    | (Some("recording"), Some("starting" | "recording" | "processing")) => {
                        ("failed", "interrupted_by_restart")
                    }
                    (Some("jira"), Some("pending")) => {
                        ("uncertain", "creation_result_unconfirmed_no_repeat_post")
                    }
                    _ => continue,
                };
                job["status"] = json!(status);
                job["error"] = json!(error);
                let mut bytes = serde_json::to_vec(&job)?;
                if bytes.len() > MAX_JOB_BYTES {
                    // A job at the previous size limit can still become terminal.
                    job.as_object_mut()
                        .context("invalid job metadata")?
                        .remove("error");
                    bytes = serde_json::to_vec(&job)?;
                }
                // Recover in place even if a legacy instance already exceeds its aggregate limit.
                super::atomic_write(&path, &self.encode_job(&instance, &id, &bytes)?)?;
                recovered += 1;
            }
        }
        Ok(recovered)
    }

    fn encode_job(&self, instance: &str, id: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        ensure!(bytes.len() <= MAX_JOB_BYTES, "job metadata too large");
        self.encrypt(bytes, format!("{instance}/{id}/job").as_bytes())
    }

    fn decode_job(&self, instance: &str, id: &str, encrypted: &[u8]) -> Result<Value> {
        let bytes = self.decrypt(encrypted, format!("{instance}/{id}/job").as_bytes())?;
        serde_json::from_slice(&bytes).context("invalid job metadata")
    }
}

fn job_file_id(entry: &fs::DirEntry) -> Result<Option<String>> {
    let name = entry.file_name();
    let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".job")) else {
        return Ok(None);
    };
    if validate_id(id).is_err() || !entry.file_type()?.is_file() {
        return Ok(None);
    }
    Ok(Some(id.to_owned()))
}

fn read_job_file(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(metadata.is_file(), "invalid job file");
    ensure!(
        metadata.len() <= MAX_JOB_FILE_BYTES,
        "job metadata too large"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(MAX_JOB_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_JOB_FILE_BYTES,
        "job metadata too large"
    );
    Ok(bytes)
}

struct JobBudget {
    count: usize,
    bytes: u64,
}

impl JobBudget {
    fn new() -> Self {
        Self { count: 0, bytes: 0 }
    }

    fn add(&mut self, bytes: u64) -> Result<()> {
        self.count += 1;
        ensure!(
            self.count <= MAX_JOBS_PER_INSTANCE,
            "too many jobs for this meeting"
        );
        self.bytes = self.bytes.saturating_add(bytes.max(MIN_JOB_CHARGE_BYTES));
        ensure!(
            self.bytes <= MAX_JOB_BYTES_PER_INSTANCE,
            "aggregate job metadata too large"
        );
        Ok(())
    }
}
fn aad(instance: &str, id: &str, len: u64, code: u8, prefix: [u8; 4]) -> Vec<u8> {
    let mut aad = format!("{instance}/{id}/stream").into_bytes();
    aad.extend_from_slice(&len.to_be_bytes());
    aad.push(code);
    aad.extend_from_slice(&prefix);
    aad
}
fn nonce(prefix: [u8; 4], index: u64) -> Nonce {
    let mut nonce = [0; 12];
    nonce[..4].copy_from_slice(&prefix);
    nonce[4..].copy_from_slice(&index.to_be_bytes());
    Nonce::assume_unique_for_key(nonce)
}
