use agentforge_sources::{EntryKind, SourceError, UntrustedEntry, VendorLimits, VendorTree};

fn validate(entries: Vec<UntrustedEntry>) -> Result<VendorTree, SourceError> {
    VendorTree::validate(entries, VendorLimits::default(), 0)
}

#[test]
fn canonical_hash_is_stable_and_paths_are_sorted() {
    let a = validate(vec![
        UntrustedEntry::file("b.txt", b"b"),
        UntrustedEntry::file("a.txt", b"a"),
    ])
    .unwrap();
    let b = validate(vec![
        UntrustedEntry::file("a.txt", b"a"),
        UntrustedEntry::file("b.txt", b"b"),
    ])
    .unwrap();
    assert_eq!(a.sha256, b.sha256);
    assert_eq!(
        a.files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["a.txt", "b.txt"]
    );
}

#[test]
fn rejects_unsafe_and_colliding_paths() {
    for path in [
        "../escape",
        "/absolute",
        "C:\\absolute",
        "dir//file",
        "dir/CON.txt",
    ] {
        assert!(
            matches!(
                validate(vec![UntrustedEntry::file(path, b"x")]),
                Err(SourceError::UnsafePath(_))
            ),
            "{path}"
        );
    }
    for paths in [["Readme.md", "README.md"], ["a\\b", "a/b"]] {
        assert!(matches!(
            validate(
                paths
                    .into_iter()
                    .map(|path| UntrustedEntry::file(path, b"x"))
                    .collect()
            ),
            Err(SourceError::PathCollision { .. })
        ));
    }
}

#[test]
fn rejects_non_regular_entries() {
    for kind in [
        EntryKind::Symlink,
        EntryKind::HardLink,
        EntryKind::Device,
        EntryKind::Fifo,
        EntryKind::Socket,
    ] {
        let entry = UntrustedEntry {
            path: "unsafe".into(),
            kind,
            mode: 0,
            content: vec![],
        };
        assert!(matches!(
            validate(vec![entry]),
            Err(SourceError::ForbiddenEntry { .. })
        ));
    }
}

#[test]
fn enforces_all_size_limits() {
    let limits = VendorLimits {
        max_files: 1,
        max_file_bytes: 2,
        max_dependency_bytes: 3,
        max_total_bytes: 4,
    };
    assert!(matches!(
        VendorTree::validate(vec![UntrustedEntry::file("a", b"abc")], limits, 0),
        Err(SourceError::FileTooLarge { .. })
    ));
    assert!(matches!(
        VendorTree::validate(
            vec![
                UntrustedEntry::file("a", b"a"),
                UntrustedEntry::file("b", b"b")
            ],
            limits,
            0
        ),
        Err(SourceError::TooManyFiles { .. })
    ));
    let more_files = VendorLimits {
        max_files: 3,
        ..limits
    };
    assert!(matches!(
        VendorTree::validate(
            vec![
                UntrustedEntry::file("a", b"aa"),
                UntrustedEntry::file("b", b"bb")
            ],
            more_files,
            0
        ),
        Err(SourceError::DependencyTooLarge { .. })
    ));
    let more_dependency = VendorLimits {
        max_dependency_bytes: 4,
        ..more_files
    };
    assert!(matches!(
        VendorTree::validate(vec![UntrustedEntry::file("a", b"aa")], more_dependency, 3),
        Err(SourceError::TotalTooLarge { .. })
    ));
}

#[test]
fn detects_executable_content_without_running_it() {
    let tree = validate(vec![
        UntrustedEntry {
            path: "bin/tool".into(),
            kind: EntryKind::Regular,
            mode: 0o755,
            content: b"data".to_vec(),
        },
        UntrustedEntry::file("script.ps1", b"Write-Host test"),
        UntrustedEntry::file("run", b"#!/bin/sh\ntrue"),
    ])
    .unwrap();
    assert!(tree.executable_content);
    assert!(tree.files.iter().all(|file| file.executable));
}
