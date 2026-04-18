use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::registry::RegistryClient;
use crate::resolver::ResolvedPackage;
use crate::workspace::WorkspaceGraph;

pub struct Installer {
    cwd: PathBuf,
}

impl Installer {
    pub fn new(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
        }
    }

    pub fn install(
        &self,
        resolved: &[ResolvedPackage],
        workspace: &WorkspaceGraph,
    ) -> anyhow::Result<()> {
        let node_modules = self.cwd.join("node_modules");
        fs::create_dir_all(&node_modules)?;
        let bin_dir = node_modules.join(".bin");
        fs::create_dir_all(&bin_dir)?;

        let registry = RegistryClient::new_default()?;
        for pkg in resolved {
            if pkg.version == "workspace" {
                if let Some(ws_pkg) = workspace.find_by_name(&pkg.name) {
                    let target = node_modules.join(&pkg.name);
                    create_symlink_dir(&ws_pkg.path, &target)?;
                }
                continue;
            }

            let version = semver::Version::parse(&pkg.version)
                .with_context(|| format!("invalid resolved version for {}", pkg.name))?;
            let tarball = registry.fetch_tarball(&pkg.name, &version)?;
            let pkg_dir = node_modules.join(&pkg.name);
            if pkg_dir.exists() {
                fs::remove_dir_all(&pkg_dir)?;
            }
            registry.extract_tarball_to(&tarball, &pkg_dir)?;
            self.link_bins(&pkg_dir, &bin_dir)?;
        }

        Ok(())
    }

    fn link_bins(&self, pkg_dir: &Path, bin_dir: &Path) -> anyhow::Result<()> {
        let manifest_path = pkg_dir.join("package.json");
        if !manifest_path.exists() {
            return Ok(());
        }
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let val: serde_json::Value =
            serde_json::from_str(&raw).context("invalid package.json in installed package")?;
        let Some(bin_val) = val.get("bin") else {
            return Ok(());
        };
        if let Some(single) = bin_val.as_str() {
            if let Some(name) = val.get("name").and_then(|v| v.as_str()) {
                let src = pkg_dir.join(single);
                if src.exists() {
                    let dst = bin_dir.join(name);
                    create_symlink_file(&src, &dst)?;
                }
            }
            return Ok(());
        }
        if let Some(obj) = bin_val.as_object() {
            for (name, rel) in obj {
                if let Some(rel) = rel.as_str() {
                    let src = pkg_dir.join(rel);
                    if src.exists() {
                        let dst = bin_dir.join(name);
                        create_symlink_file(&src, &dst)?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn create_symlink_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    if dst.exists() {
        fs::remove_file(dst)
            .or_else(|_| fs::remove_dir_all(dst))
            .ok();
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    symlink(src, dst)?;
    Ok(())
}

#[cfg(windows)]
fn create_symlink_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    use std::os::windows::fs::symlink_dir;
    if dst.exists() {
        fs::remove_dir_all(dst)
            .or_else(|_| fs::remove_file(dst))
            .ok();
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    symlink_dir(src, dst)?;
    Ok(())
}

#[cfg(unix)]
fn create_symlink_file(src: &Path, dst: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    if dst.exists() {
        fs::remove_file(dst).ok();
    }
    symlink(src, dst)?;
    Ok(())
}

#[cfg(windows)]
fn create_symlink_file(src: &Path, dst: &Path) -> anyhow::Result<()> {
    use std::os::windows::fs::symlink_file;
    if dst.exists() {
        fs::remove_file(dst).ok();
    }
    symlink_file(src, dst)?;
    Ok(())
}
