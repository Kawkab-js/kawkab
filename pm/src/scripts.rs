use std::path::Path;
use std::process::Command;

use anyhow::Context;

use crate::manifest::Manifest;

pub struct ScriptRunner {
    cwd: std::path::PathBuf,
}

impl ScriptRunner {
    pub fn new(cwd: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            cwd: cwd.to_path_buf(),
        })
    }

    pub fn run(&self, manifest: &Manifest, name: &str, args: &[String]) -> anyhow::Result<()> {
        let cmd = manifest
            .scripts
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("script not found: {name}"))?;

        let mut full = cmd.clone();
        if !args.is_empty() {
            full.push(' ');
            full.push_str(&args.join(" "));
        }

        let mut command = Command::new("bash");
        command.arg("-lc").arg(full).current_dir(&self.cwd);

        let node_bin = self.cwd.join("node_modules").join(".bin");
        let old_path = std::env::var("PATH").unwrap_or_default();
        let mut new_path = node_bin.to_string_lossy().to_string();
        if !old_path.is_empty() {
            new_path.push(':');
            new_path.push_str(&old_path);
        }
        command.env("PATH", new_path);

        let status = command.status().context("failed to launch script process")?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!("script failed: {name} with status {status}")
        }
    }
}
