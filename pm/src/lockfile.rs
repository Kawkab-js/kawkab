use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LockPackage {
    pub name: String,
    pub version: String,
    pub resolved: String,
    pub integrity: String,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "peerDependencies")]
    pub peer_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub dev: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KawkabLock {
    pub version: u32,
    pub root: String,
    #[serde(default)]
    pub packages: Vec<LockPackage>,
}

impl KawkabLock {
    pub fn new() -> Self {
        Self {
            version: 1,
            root: "root".to_string(),
            packages: Vec::new(),
        }
    }

    pub fn load(cwd: &Path) -> anyhow::Result<Self> {
        let file = cwd.join("kawkab.lock");
        let raw =
            std::fs::read_to_string(&file).with_context(|| format!("failed to read {}", file.display()))?;
        let parsed: KawkabLock = serde_json::from_str(&raw).context("failed to parse kawkab.lock")?;
        Ok(parsed)
    }

    pub fn save(&self, cwd: &Path) -> anyhow::Result<()> {
        let file = cwd.join("kawkab.lock");
        let encoded = serde_json::to_string_pretty(self)?;
        std::fs::write(&file, format!("{encoded}\n"))
            .with_context(|| format!("failed to write {}", file.display()))?;
        Ok(())
    }
}
