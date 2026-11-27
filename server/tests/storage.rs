use ecochain_rtc_server::storage::{Archive, Vault};
use serde_json::json;
use std::{collections::BTreeSet, fs};

#[test]
fn encrypted_journal_replays_after_restart_and_ignores_torn_tail() {
    let root = std::env::temp_dir().join(format!("ecochain-vault-{}", uuid::Uuid::new_v4()));
    let instance = uuid::Uuid::new_v4().to_string();
    let vault = Vault::open(&root, [7; 32], 86400).unwrap();
    let archive = Archive {
        state: json!({"instance_id":instance,"revision":1,"notes":[{"text":"private roadmap"}]}),
        members: BTreeSet::from(["alice".to_owned()]),
        updated_at: ecochain_rtc_server::auth::unix_now(),
    };
    vault.append(&archive).unwrap();
    let journal = root.join(&instance).join("workspace.journal");
    let raw = fs::read(&journal).unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("private roadmap"));
    drop(vault);
    let mut file = fs::OpenOptions::new().append(true).open(&journal).unwrap();
    use std::io::Write;
    file.write_all(&[0, 0]).unwrap();
    drop(file);
    let reopened = Vault::open(&root, [7; 32], 86400).unwrap();
    assert_eq!(reopened.read(&instance).unwrap().state, archive.state);
    let mut newer = archive.clone();
    newer.state["revision"] = json!(2);
    reopened.append(&newer).unwrap();
    assert_eq!(reopened.read(&instance).unwrap().state["revision"], 2);
    assert!(Vault::open(&root, [8; 32], 86400)
        .unwrap()
        .read(&instance)
        .is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn attachment_ciphertext_is_bound_to_instance_and_rejects_tampering() {
    let root = std::env::temp_dir().join(format!("ecochain-files-{}", uuid::Uuid::new_v4()));
    let instance = uuid::Uuid::new_v4().to_string();
    let vault = Vault::open(&root, [9; 32], 86400).unwrap();
    let id = vault.put(&instance, b"private bytes").unwrap();
    assert_eq!(vault.get(&instance, &id).unwrap(), b"private bytes");
    assert!(vault.get("../../etc", &id).is_err());
    assert!(vault.get(&uuid::Uuid::new_v4().to_string(), &id).is_err());
    let path = root.join(&instance).join(format!("{id}.blob"));
    let mut raw = fs::read(&path).unwrap();
    let n = raw.len();
    raw[n - 1] ^= 1;
    fs::write(path, raw).unwrap();
    assert!(vault.get(&instance, &id).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn encrypted_stream_supports_authenticated_ranges_across_chunk_boundaries() {
    let root = std::env::temp_dir().join(format!("ecochain-stream-{}", uuid::Uuid::new_v4()));
    let instance = uuid::Uuid::new_v4().to_string();
    let vault = Vault::open(&root, [5; 32], 86400).unwrap();
    let source = root.join("source.mp4");
    let bytes: Vec<_> = (0..150_123).map(|i| (i % 251) as u8).collect();
    fs::write(&source, &bytes).unwrap();
    let id = vault.put_file(&instance, &source, "video/mp4").unwrap();
    assert_eq!(
        vault.stream_metadata(&instance, &id).unwrap(),
        (bytes.len() as u64, "video/mp4")
    );
    assert_eq!(
        vault.read_range(&instance, &id, 65_500, 1000).unwrap(),
        bytes[65_500..66_500]
    );
    assert_eq!(
        vault.read_range(&instance, &id, 149_999, 124).unwrap(),
        bytes[149_999..]
    );
    assert!(vault.read_range(&instance, &id, 150_123, 1).is_err());
    let path = root.join(&instance).join(format!("{id}.stream"));
    let mut encrypted = fs::read(&path).unwrap();
    encrypted[15] ^= 1;
    fs::write(&path, encrypted).unwrap();
    assert!(vault.stream_metadata(&instance, &id).is_err());
    fs::remove_dir_all(root).unwrap();
}
