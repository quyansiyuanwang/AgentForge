use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Read-only filesystem seam used by detectors, validation, and planning.
/// Mutation is intentionally reserved for the transactional applier.
pub trait FileSystem: Send + Sync {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn metadata(&self, path: &Path) -> io::Result<fs::Metadata>;
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = fs::read_dir(path)?
            .map(|entry| entry.map(|value| value.path()))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort();
        Ok(entries)
    }

    fn metadata(&self, path: &Path) -> io::Result<fs::Metadata> {
        fs::symlink_metadata(path)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }
}
