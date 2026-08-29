use std::path::Path;

use agentforge_core::filesystem::{FileSystem, RealFileSystem};

#[test]
fn real_filesystem_returns_directory_entries_in_stable_order() {
    let directory = tempfile::tempdir().expect("temporary directory");
    std::fs::write(directory.path().join("z.txt"), b"z").expect("write z");
    std::fs::write(directory.path().join("a.txt"), b"a").expect("write a");

    let entries = RealFileSystem
        .read_dir(directory.path())
        .expect("read directory");
    let names = entries
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(names, ["a.txt", "z.txt"]);
    assert_eq!(RealFileSystem.read(&entries[0]).unwrap(), b"a");
    assert!(RealFileSystem.metadata(&entries[0]).unwrap().is_file());
    assert_eq!(
        RealFileSystem
            .canonicalize(Path::new(directory.path()))
            .unwrap(),
        std::fs::canonicalize(directory.path()).unwrap()
    );
}
