use std::path::{Path, PathBuf};

/// Lightweight async I/O driver used by the `io` crate.
///
/// This keeps a stable API while the full io_uring pipeline evolves.
pub struct UringDriver {
    root: PathBuf,
}

impl UringDriver {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub async fn read_file(&self, path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
        tokio::fs::read(self.root.join(path.as_ref())).await
    }

    pub async fn write_file(&self, path: impl AsRef<Path>, data: &[u8]) -> std::io::Result<()> {
        let full_path = self.root.join(path.as_ref());
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(full_path, data).await
    }
}
