use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use semver::Version;

use crate::manifest::Manifest;
use crate::registry::{matches_version_req, RegistryClient};
use crate::workspace::WorkspaceGraph;

#[derive(Clone, Debug)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: String,
    pub resolved: String,
    pub integrity: String,
    pub dependencies: BTreeMap<String, String>,
    pub peer_dependencies: BTreeMap<String, String>,
    pub dev: bool,
}

pub struct Resolver {
    registry: RegistryClient,
}

impl Resolver {
    pub fn new(registry: RegistryClient) -> Self {
        Self { registry }
    }

    pub fn resolve_workspace(
        &mut self,
        _cwd: &Path,
        root_manifest: &Manifest,
        workspace: &WorkspaceGraph,
    ) -> anyhow::Result<Vec<ResolvedPackage>> {
        let mut queue = Vec::<(String, String, bool)>::new();
        for (name, req) in root_manifest.all_prod_dependencies() {
            queue.push((name.to_string(), req.to_string(), false));
        }
        for (name, req) in root_manifest.dev_dependencies.iter() {
            queue.push((name.clone(), req.clone(), true));
        }

        for ws in &workspace.packages {
            let manifest = Manifest::load(&ws.path)?;
            for (name, req) in manifest.all_prod_dependencies() {
                queue.push((name.to_string(), req.to_string(), false));
            }
            for (name, req) in manifest.dev_dependencies {
                queue.push((name, req, true));
            }
        }

        queue.sort();
        queue.dedup();

        let mut seen = BTreeSet::<String>::new();
        let mut resolved = Vec::<ResolvedPackage>::new();

        while let Some((name, req, dev)) = queue.pop() {
            let key = format!("{name}@{req}");
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);

            if workspace.contains_name(&name) {
                let ws_version = workspace.version_of(&name).unwrap_or("0.0.0").to_string();
                if is_workspace_selector(&req) && !workspace_selector_matches(&req, &ws_version) {
                    anyhow::bail!(
                        "workspace selector conflict: required {}@{} but local workspace version is {}",
                        name,
                        req,
                        ws_version
                    );
                }
                resolved.push(ResolvedPackage {
                    name: name.clone(),
                    version: ws_version,
                    resolved: "workspace".to_string(),
                    integrity: "workspace".to_string(),
                    dependencies: BTreeMap::new(),
                    peer_dependencies: BTreeMap::new(),
                    dev,
                });
                continue;
            }

            let picked: Version = self.registry.pick_version(&name, &req)?;
            let (deps, peer_deps, tarball_url, integrity) =
                self.registry.fetch_version_doc(&name, &picked)?;
            for (dep_name, dep_req) in deps.iter().chain(peer_deps.iter()) {
                if dep_req.starts_with("workspace:") && workspace.contains_name(dep_name) {
                    let ws_version = workspace.version_of(dep_name).unwrap_or("0.0.0");
                    if !workspace_selector_matches(dep_req, ws_version) {
                        anyhow::bail!(
                            "workspace selector conflict: {} requires {}@{} but local workspace version is {}",
                            name,
                            dep_name,
                            dep_req,
                            ws_version
                        );
                    }
                    continue;
                }
                queue.push((dep_name.clone(), dep_req.clone(), dev));
            }
            resolved.push(ResolvedPackage {
                name,
                version: picked.to_string(),
                resolved: tarball_url,
                integrity,
                dependencies: deps,
                peer_dependencies: peer_deps,
                dev,
            });
        }

        validate_peer_dependencies(&resolved, workspace)?;
        resolved.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
        Ok(resolved)
    }
}

fn validate_peer_dependencies(
    resolved: &[ResolvedPackage],
    workspace: &WorkspaceGraph,
) -> anyhow::Result<()> {
    let mut chosen = BTreeMap::<String, String>::new();
    for pkg in resolved {
        chosen
            .entry(pkg.name.clone())
            .or_insert_with(|| pkg.version.clone());
    }

    let mut errors = Vec::new();
    for pkg in resolved {
        for (peer_name, peer_req) in &pkg.peer_dependencies {
            if peer_req.starts_with("workspace:") {
                if !workspace.contains_name(peer_name) {
                    errors.push(format!(
                        "peer conflict: {}@{} requires {}@{} but workspace package not found (path: {} -> {})",
                        pkg.name,
                        pkg.version,
                        peer_name,
                        peer_req,
                        pkg.name,
                        peer_name
                    ));
                }
                continue;
            }

            let Some(chosen_ver) = chosen.get(peer_name) else {
                errors.push(format!(
                    "peer conflict: {}@{} requires {}@{} but no compatible version is installed (path: {} -> {})",
                    pkg.name,
                    pkg.version,
                    peer_name,
                    peer_req,
                    pkg.name,
                    peer_name
                ));
                continue;
            };
            let Ok(v) = Version::parse(chosen_ver) else {
                continue;
            };
            if !matches_version_req(peer_req, &v) {
                errors.push(format!(
                    "peer conflict: {}@{} requires {}@{} but resolved {}@{} (path: {} -> {})",
                    pkg.name,
                    pkg.version,
                    peer_name,
                    peer_req,
                    peer_name,
                    chosen_ver,
                    pkg.name,
                    peer_name
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(errors.join("\n"))
    }
}

fn is_workspace_selector(req: &str) -> bool {
    req.trim().starts_with("workspace:")
}

fn workspace_selector_matches(selector: &str, local_version: &str) -> bool {
    let Some(rest) = selector.trim().strip_prefix("workspace:") else {
        return true;
    };
    let normalized = if rest.is_empty() || rest == "*" {
        "*".to_string()
    } else if rest == "^" || rest == "~" {
        format!("{rest}{local_version}")
    } else {
        rest.to_string()
    };

    if normalized == "*" {
        return true;
    }

    let Ok(v) = Version::parse(local_version) else {
        return false;
    };
    matches_version_req(&normalized, &v)
}
