use std::fs;

use agentforge_sources::{
    ContentKind, LockEntry, LockFile, SourceType, UntrustedEntry, VendorLimits, VendorTree,
    verify_vendor_offline,
};

fn entry(id: &str, tree: &VendorTree) -> LockEntry {
    LockEntry {
        kind: ContentKind::Skill,
        id: id.into(),
        source_type: SourceType::Url,
        requested_locator: "https://example.com".into(),
        resolved_locator: "https://example.com/final".into(),
        resolved_version: None,
        git_commit: None,
        tree_hash: None,
        etag: None,
        last_modified: None,
        vendor_path: format!(".agentforge/vendor/skills/{id}"),
        sha256: tree.sha256.clone(),
        executable_content: tree.executable_content,
        file_count: tree.files.len() as u64,
        total_bytes: tree.total_bytes,
    }
}

#[test]
fn lock_is_sorted_stable_and_has_no_timestamp() {
    let tree = VendorTree::validate(
        vec![UntrustedEntry::file("content", b"x")],
        VendorLimits::default(),
        0,
    )
    .unwrap();
    let lock = LockFile::new(
        "agentforge 0.1.0",
        vec![entry("z", &tree), entry("a", &tree)],
    )
    .unwrap();
    let first = lock.to_yaml().unwrap();
    assert_eq!(first, lock.to_yaml().unwrap());
    assert!(first.find("id: a").unwrap() < first.find("id: z").unwrap());
    assert!(!first.to_ascii_lowercase().contains("timestamp"));
}

#[test]
fn offline_verifier_accepts_exact_tree_and_rejects_drift_without_network() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join(".agentforge/vendor/skills/testing");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("content"), b"hello").unwrap();
    let tree = VendorTree::validate(
        vec![UntrustedEntry::file("content", b"hello")],
        VendorLimits::default(),
        0,
    )
    .unwrap();
    let lock = LockFile::new("agentforge 0.1.0", vec![entry("testing", &tree)]).unwrap();
    verify_vendor_offline(root.path(), &lock, VendorLimits::default()).unwrap();
    fs::write(directory.join("content"), b"drift").unwrap();
    assert!(verify_vendor_offline(root.path(), &lock, VendorLimits::default()).is_err());
}

#[test]
fn offline_verifier_rejects_hard_links() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join(".agentforge/vendor/skills/testing");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("content"), b"hello").unwrap();
    fs::hard_link(directory.join("content"), directory.join("alias")).unwrap();
    let tree = VendorTree::validate(
        vec![
            UntrustedEntry::file("alias", b"hello"),
            UntrustedEntry::file("content", b"hello"),
        ],
        VendorLimits::default(),
        0,
    )
    .unwrap();
    let lock = LockFile::new("agentforge 0.1.0", vec![entry("testing", &tree)]).unwrap();
    assert!(verify_vendor_offline(root.path(), &lock, VendorLimits::default()).is_err());
}
