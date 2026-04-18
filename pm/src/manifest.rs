use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{registry::RegistryClient, UpdateStrategy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencySection {
    Dependencies,
    DevDependencies,
    PeerDependencies,
    OptionalDependencies,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub scripts: BTreeMap<String, String>,
    #[serde(default, rename = "workspaces")]
    pub workspace_patterns: Vec<String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    pub dev_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "peerDependencies")]
    pub peer_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "optionalDependencies")]
    pub optional_dependencies: BTreeMap<String, String>,
}

impl Manifest {
    pub fn load(cwd: &Path) -> anyhow::Result<Self> {
        let file = cwd.join("package.json");
        let raw = std::fs::read_to_string(&file)
            .with_context(|| format!("failed to read {}", file.display()))?;
        let manifest: Manifest =
            serde_json::from_str(&raw).context("failed to parse package.json")?;
        Ok(manifest)
    }

    pub fn save(&self, cwd: &Path) -> anyhow::Result<()> {
        let file = cwd.join("package.json");
        let encoded = serde_json::to_string_pretty(self)?;
        std::fs::write(&file, format!("{encoded}\n"))
            .with_context(|| format!("failed to write {}", file.display()))?;
        Ok(())
    }

    pub fn add_dependency(&mut self, name: &str, range: &str, section: DependencySection) {
        match section {
            DependencySection::Dependencies => {
                self.dependencies
                    .insert(name.to_string(), range.to_string());
            }
            DependencySection::DevDependencies => {
                self.dev_dependencies
                    .insert(name.to_string(), range.to_string());
            }
            DependencySection::PeerDependencies => {
                self.peer_dependencies
                    .insert(name.to_string(), range.to_string());
            }
            DependencySection::OptionalDependencies => {
                self.optional_dependencies
                    .insert(name.to_string(), range.to_string());
            }
        }
    }

    pub fn remove_dependency(&mut self, name: &str) {
        let _ = self.dependencies.remove(name);
        let _ = self.dev_dependencies.remove(name);
        let _ = self.peer_dependencies.remove(name);
        let _ = self.optional_dependencies.remove(name);
    }

    pub fn all_direct_dependencies(&self) -> impl Iterator<Item = (&str, &str)> {
        self.dependencies
            .iter()
            .chain(self.dev_dependencies.iter())
            .chain(self.optional_dependencies.iter())
            .map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn all_prod_dependencies(&self) -> impl Iterator<Item = (&str, &str)> {
        self.dependencies
            .iter()
            .chain(self.optional_dependencies.iter())
            .map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn bump_versions(&mut self, cwd: &Path, strategy: UpdateStrategy) -> anyhow::Result<()> {
        let registry = RegistryClient::new_default()?;

        fn bump_map(
            map: &mut BTreeMap<String, String>,
            registry: &RegistryClient,
            strategy: &UpdateStrategy,
        ) -> anyhow::Result<()> {
            let keys = map.keys().cloned().collect::<Vec<_>>();
            for name in keys {
                let current = map
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing dep in map"))?;
                let latest = registry.latest_version(&name)?;
                let picked = match strategy {
                    UpdateStrategy::Latest => latest.to_string(),
                    UpdateStrategy::Minor => keep_major_update_minor(&current, &latest.to_string()),
                    UpdateStrategy::Patch => keep_minor_update_patch(&current, &latest.to_string()),
                };
                map.insert(name, picked);
            }
            Ok(())
        }

        bump_map(&mut self.dependencies, &registry, &strategy)?;
        bump_map(&mut self.dev_dependencies, &registry, &strategy)?;
        bump_map(&mut self.optional_dependencies, &registry, &strategy)?;

        if cwd.join("package.json").exists() {
            Ok(())
        } else {
            anyhow::bail!("package.json not found at {}", cwd.display())
        }
    }
}

fn keep_major_update_minor(current: &str, latest: &str) -> String {
    let cur = parse_semver_loose(current);
    let lat = parse_semver_loose(latest);
    if cur.0 != lat.0 {
        current.to_string()
    } else {
        format!("^{}.{}.{}", lat.0, lat.1, lat.2)
    }
}

fn keep_minor_update_patch(current: &str, latest: &str) -> String {
    let cur = parse_semver_loose(current);
    let lat = parse_semver_loose(latest);
    if cur.0 != lat.0 || cur.1 != lat.1 {
        current.to_string()
    } else {
        format!("~{}.{}.{}", lat.0, lat.1, lat.2)
    }
}

fn parse_semver_loose(s: &str) -> (u64, u64, u64) {
    let trimmed = s.trim_start_matches('^').trim_start_matches('~');
    let mut parts = trimmed.split('.');
    let major = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|v| v.split('-').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    (major, minor, patch)
}
