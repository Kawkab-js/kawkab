use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::manifest::Manifest;

#[derive(Clone, Debug)]
pub struct WorkspacePackage {
    pub name: String,
    pub version: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceGraph {
    pub root: PathBuf,
    pub packages: Vec<WorkspacePackage>,
}

impl WorkspaceGraph {
    pub fn discover(cwd: &Path, manifest: &Manifest) -> anyhow::Result<Self> {
        let mut graph = WorkspaceGraph {
            root: cwd.to_path_buf(),
            packages: Vec::new(),
        };
        if manifest.workspace_patterns.is_empty() {
            return Ok(graph);
        }

        for pattern in &manifest.workspace_patterns {
            if pattern.ends_with("/*") {
                let parent = cwd.join(pattern.trim_end_matches("/*"));
                if parent.exists() {
                    for entry in std::fs::read_dir(parent)? {
                        let entry = entry?;
                        let path = entry.path();
                        if path.join("package.json").exists() {
                            let sub = Manifest::load(&path)?;
                            let name = sub.name.unwrap_or_else(|| {
                                path.file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string()
                            });
                            let version = sub.version.unwrap_or_else(|| "0.0.0".to_string());
                            graph.packages.push(WorkspacePackage {
                                name,
                                version,
                                path,
                            });
                        }
                    }
                }
            } else {
                let exact = cwd.join(pattern);
                if exact.join("package.json").exists() {
                    let sub = Manifest::load(&exact)?;
                    let name = sub.name.unwrap_or_else(|| {
                        exact
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    });
                    let version = sub.version.unwrap_or_else(|| "0.0.0".to_string());
                    graph.packages.push(WorkspacePackage {
                        name,
                        version,
                        path: exact,
                    });
                }
            }
        }

        graph.packages.sort_by(|a, b| a.name.cmp(&b.name));
        graph.packages.dedup_by(|a, b| a.path == b.path);
        Ok(graph)
    }

    pub fn contains_name(&self, name: &str) -> bool {
        self.packages.iter().any(|p| p.name == name)
    }

    pub fn find_by_name(&self, name: &str) -> Option<&WorkspacePackage> {
        self.packages.iter().find(|p| p.name == name)
    }

    pub fn version_of(&self, name: &str) -> Option<&str> {
        self.find_by_name(name).map(|p| p.version.as_str())
    }

    pub fn find_nearest_root(start: &Path) -> PathBuf {
        for entry in WalkDir::new(start).max_depth(1) {
            let _ = entry;
        }
        start.to_path_buf()
    }
}
