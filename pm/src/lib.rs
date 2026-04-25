pub mod installer;
pub mod lockfile;
pub mod manifest;
pub mod registry;
pub mod resolver;
pub mod scripts;
pub mod workspace;

use anyhow::Context;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::installer::Installer;
use crate::lockfile::{KawkabLock, LockPackage};
use crate::manifest::{DependencySection, Manifest};
use crate::registry::{matches_version_req, RegistryClient};
use crate::resolver::Resolver;
use crate::scripts::ScriptRunner;
use crate::workspace::WorkspaceGraph;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStrategy {
    Latest,
    Minor,
    Patch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PmCommand {
    Install,
    Add {
        name: String,
        range: String,
        section: DependencySection,
    },
    Remove {
        name: String,
    },
    Update {
        strategy: UpdateStrategy,
    },
    Run {
        script: String,
        args: Vec<String>,
    },
    Outdated,
    Why {
        name: String,
        json: bool,
        pretty: bool,
        schema: bool,
    },
    Doctor {
        json: bool,
        pretty: bool,
    },
    Init {
        force: bool,
        entry: String,
    },
}

pub fn execute_command(cwd: &Path, command: PmCommand) -> anyhow::Result<()> {
    if let PmCommand::Init { force, entry } = command {
        return run_init(cwd, force, &entry);
    }

    let mut manifest = Manifest::load(cwd).context("failed to read package.json")?;
    let workspace = WorkspaceGraph::discover(cwd, &manifest)?;

    match command {
        PmCommand::Add {
            name,
            range,
            section,
        } => {
            manifest.add_dependency(&name, &range, section);
            manifest.save(cwd)?;
            install_workspace(cwd, &manifest, &workspace)?;
        }
        PmCommand::Remove { name } => {
            manifest.remove_dependency(&name);
            manifest.save(cwd)?;
            install_workspace(cwd, &manifest, &workspace)?;
        }
        PmCommand::Install => {
            install_workspace(cwd, &manifest, &workspace)?;
        }
        PmCommand::Update { strategy } => {
            manifest.bump_versions(cwd, strategy)?;
            manifest.save(cwd)?;
            install_workspace(cwd, &manifest, &workspace)?;
        }
        PmCommand::Run { script, args } => {
            let runner = ScriptRunner::new(cwd)?;
            runner.run(&manifest, &script, &args)?;
        }
        PmCommand::Outdated => {
            let registry = RegistryClient::new_default()?;
            for (name, req) in manifest.all_direct_dependencies() {
                let latest = registry.latest_version(name)?;
                if latest.to_string() != req {
                    println!("{name}: current={req} latest={latest}");
                }
            }
        }
        PmCommand::Why {
            name,
            json,
            pretty,
            schema,
        } => {
            let lock = KawkabLock::load(cwd).unwrap_or_else(|_| KawkabLock::new());
            if schema {
                let schema_doc = why_report_schema();
                if pretty {
                    println!("{}", serde_json::to_string_pretty(&schema_doc)?);
                } else {
                    println!("{}", serde_json::to_string(&schema_doc)?);
                }
            } else {
                let report = build_why_report(&lock, &name);
                if json {
                    if pretty {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!("{}", serde_json::to_string(&report)?);
                    }
                } else {
                    let lines = render_why_tree(&report);
                    if report.entries.is_empty() {
                        println!("No entries found for package: {name}");
                    } else {
                        for line in lines {
                            println!("{line}");
                        }
                    }
                }
            }
        }
        PmCommand::Doctor { json, pretty } => {
            let report = run_doctor(cwd, &workspace);
            if json {
                if pretty {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("{}", serde_json::to_string(&report)?);
                }
            } else {
                println!("kawkab doctor");
                println!("  root: {}", report.project_root);
                for check in &report.checks {
                    let status = if check.ok { "ok" } else { "fail" };
                    println!("  - {}: {} ({})", check.name, status, check.message);
                }
                println!(
                    "  summary: {}/{} checks passed",
                    report.passed, report.total
                );
            }
        }
        PmCommand::Init { .. } => unreachable!("init handled above"),
    }
    Ok(())
}

fn run_init(cwd: &Path, force: bool, entry: &str) -> anyhow::Result<()> {
    let pkg_path = cwd.join("package.json");
    if pkg_path.exists() && !force {
        anyhow::bail!(
            "package.json already exists (pass --force to overwrite): {}",
            pkg_path.display()
        );
    }

    let entry = entry.trim();
    if entry.is_empty() || entry.contains('/') || entry.contains('\\') {
        anyhow::bail!("init --entry must be a single file name (e.g. index.js), got: {entry:?}");
    }

    let name = infer_package_name(cwd);
    let mut manifest = Manifest {
        name: Some(name.clone()),
        version: Some("1.0.0".to_string()),
        scripts: Default::default(),
        workspace_patterns: Vec::new(),
        dependencies: Default::default(),
        dev_dependencies: Default::default(),
        peer_dependencies: Default::default(),
        optional_dependencies: Default::default(),
    };
    let start_cmd = format!("kawkab --file {entry}");
    manifest.scripts.insert("start".to_string(), start_cmd);
    manifest.save(cwd)?;

    let entry_path = cwd.join(entry);
    if !entry_path.exists() {
        fs::write(&entry_path, "console.log(\"Hello from Kawkab\");\n")
            .with_context(|| format!("failed to write {}", entry_path.display()))?;
        println!("Created {}", entry_path.display());
    }

    println!("Wrote {} (package {name})", pkg_path.display());
    println!("Run: kawkab run start   or: kawkab --file {entry}");
    Ok(())
}

fn infer_package_name(cwd: &Path) -> String {
    let fallback = "my-package".to_string();
    let Some(raw) = cwd.file_name().and_then(|n| n.to_str()) else {
        return fallback;
    };
    if raw.is_empty() {
        return fallback;
    }

    let mut out = String::new();
    for c in raw.to_lowercase().chars() {
        match c {
            'a'..='z' | '0'..='9' | '.' | '_' | '-' => out.push(c),
            ' ' | '\t' => out.push('-'),
            _ => {}
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches(|c| c == '-' || c == '.');
    if out.is_empty() {
        return fallback;
    }
    let mut out = out.to_string();
    while out.starts_with('.') || out.starts_with('_') {
        out.remove(0);
    }
    if out.is_empty() {
        return fallback;
    }
    if out.len() > 214 {
        out.truncate(214);
    }
    out
}

fn install_workspace(
    cwd: &Path,
    manifest: &Manifest,
    workspace: &WorkspaceGraph,
) -> anyhow::Result<()> {
    let registry = RegistryClient::new_default()?;
    let mut resolver = Resolver::new(registry);
    let resolved = resolver.resolve_workspace(cwd, manifest, workspace)?;

    let installer = Installer::new(cwd);
    installer.install(&resolved, workspace)?;

    let lock = KawkabLock {
        version: 1,
        root: manifest.name.clone().unwrap_or_else(|| "root".to_string()),
        packages: resolved
            .iter()
            .map(|pkg| LockPackage {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                integrity: pkg.integrity.clone(),
                resolved: pkg.resolved.clone(),
                dependencies: pkg.dependencies.clone(),
                peer_dependencies: pkg.peer_dependencies.clone(),
                dev: pkg.dev,
            })
            .collect(),
    };
    lock.save(cwd)?;
    Ok(())
}

pub fn default_project_dir(cwd: &Path) -> PathBuf {
    cwd.to_path_buf()
}

#[derive(Debug, Serialize)]
struct WhyReport {
    schema_version: String,
    package: String,
    entries: Vec<WhyEntry>,
}

#[derive(Debug, Serialize)]
struct WhyEntry {
    name: String,
    version: String,
    required_by: Vec<WhyRequiredByNode>,
    peer_requirements: Vec<WhyPeerRequirement>,
    peer_conflicts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct WhyRequiredByNode {
    name: String,
    version: String,
    requested: String,
    required_by_path: Vec<String>,
}

#[derive(Debug, Serialize)]
struct WhyPeerRequirement {
    name: String,
    version: String,
    requested: String,
}

fn build_why_report(lock: &KawkabLock, target: &str) -> WhyReport {
    let entries = lock
        .packages
        .iter()
        .filter(|p| p.name == target)
        .map(|pkg| {
            let resolved_version = semver::Version::parse(&pkg.version).ok();
            let required_by = collect_required_by(lock, pkg);
            let peer_requirements = collect_peer_requirements(lock, pkg);
            let peer_conflicts = peer_requirements
                .iter()
                .filter_map(|p| {
                    if p.requested == "*" {
                        return None;
                    }
                    if p.requested.starts_with("workspace:") {
                        return None;
                    }
                    if resolved_version
                        .as_ref()
                        .map(|v| matches_version_req(&p.requested, v))
                        .unwrap_or_else(|| p.requested == pkg.version)
                    {
                        None
                    } else {
                        Some(format!(
                            "{}@{} expects {}@{} but resolved {}@{}",
                            p.name, p.version, pkg.name, p.requested, pkg.name, pkg.version
                        ))
                    }
                })
                .collect::<Vec<_>>();

            WhyEntry {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                required_by,
                peer_requirements,
                peer_conflicts,
            }
        })
        .collect::<Vec<_>>();

    WhyReport {
        schema_version: "why-report.v1".to_string(),
        package: target.to_string(),
        entries,
    }
}

fn collect_required_by(lock: &KawkabLock, pkg: &LockPackage) -> Vec<WhyRequiredByNode> {
    let mut nodes = Vec::new();
    let mut visited = BTreeSet::new();
    visited.insert(format!("{}@{}", pkg.name, pkg.version));
    collect_required_by_recursive(
        lock,
        pkg,
        vec![format!("{}@{}", pkg.name, pkg.version)],
        &mut visited,
        &mut nodes,
    );
    nodes
}

fn collect_required_by_recursive(
    lock: &KawkabLock,
    pkg: &LockPackage,
    path: Vec<String>,
    visited: &mut BTreeSet<String>,
    out: &mut Vec<WhyRequiredByNode>,
) {
    let dependents = lock
        .packages
        .iter()
        .filter(|candidate| candidate.dependencies.contains_key(&pkg.name))
        .collect::<Vec<_>>();
    if dependents.is_empty() {
        return;
    }
    for dep in dependents {
        let dep_key = format!("{}@{}", dep.name, dep.version);
        let requested = dep
            .dependencies
            .get(&pkg.name)
            .cloned()
            .unwrap_or_else(|| "?".to_string());
        let mut next_path = path.clone();
        next_path.push(dep_key.clone());
        out.push(WhyRequiredByNode {
            name: dep.name.clone(),
            version: dep.version.clone(),
            requested,
            required_by_path: next_path.clone(),
        });
        if !visited.contains(&dep_key) {
            visited.insert(dep_key.clone());
            collect_required_by_recursive(lock, dep, next_path, visited, out);
        }
    }
}

fn collect_peer_requirements(lock: &KawkabLock, pkg: &LockPackage) -> Vec<WhyPeerRequirement> {
    lock.packages
        .iter()
        .filter_map(|candidate| {
            candidate
                .peer_dependencies
                .get(&pkg.name)
                .map(|requested| WhyPeerRequirement {
                    name: candidate.name.clone(),
                    version: candidate.version.clone(),
                    requested: requested.clone(),
                })
        })
        .collect::<Vec<_>>()
}

fn render_why_tree(report: &WhyReport) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in &report.entries {
        lines.push(format!("{}@{}", entry.name, entry.version));
        if entry.required_by.is_empty() {
            lines.push("  └─ direct/root".to_string());
        } else {
            for dep in &entry.required_by {
                lines.push(format!(
                    "  └─ required by {}@{} (dep: {})",
                    dep.name, dep.version, dep.requested
                ));
                lines.push(format!("     path: {}", dep.required_by_path.join(" -> ")));
            }
        }
        if !entry.peer_requirements.is_empty() {
            lines.push("  peer requirements:".to_string());
            for p in &entry.peer_requirements {
                lines.push(format!(
                    "    - {}@{} expects {}@{}",
                    p.name, p.version, entry.name, p.requested
                ));
            }
        }
        if !entry.peer_conflicts.is_empty() {
            lines.push("  peer conflicts:".to_string());
            for c in &entry.peer_conflicts {
                lines.push(format!("    - {c}"));
            }
        }
    }
    lines
}

#[derive(Debug, Serialize)]
struct WhySchemaDoc {
    schema_name: String,
    schema_version: String,
    json_type: String,
    required: Vec<String>,
    fields: Vec<SchemaField>,
}

#[derive(Debug, Serialize)]
struct SchemaField {
    name: String,
    json_type: String,
    required: bool,
    description: String,
}

fn why_report_schema() -> WhySchemaDoc {
    WhySchemaDoc {
        schema_name: "kawkab.why-report".to_string(),
        schema_version: "1.0.0".to_string(),
        json_type: "object".to_string(),
        required: vec![
            "schema_version".to_string(),
            "package".to_string(),
            "entries".to_string(),
        ],
        fields: vec![
            SchemaField {
                name: "schema_version".to_string(),
                json_type: "string".to_string(),
                required: true,
                description: "schema id for compatibility checks".to_string(),
            },
            SchemaField {
                name: "package".to_string(),
                json_type: "string".to_string(),
                required: true,
                description: "query package name".to_string(),
            },
            SchemaField {
                name: "entries".to_string(),
                json_type: "array".to_string(),
                required: true,
                description: "resolved entries for requested package".to_string(),
            },
        ],
    }
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    schema_version: String,
    project_root: String,
    checks: Vec<DoctorCheck>,
    passed: usize,
    total: usize,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    message: String,
}

fn run_doctor(cwd: &Path, workspace: &WorkspaceGraph) -> DoctorReport {
    let mut checks = Vec::new();
    let package_json = cwd.join("package.json");
    checks.push(DoctorCheck {
        name: "package_json_exists".to_string(),
        ok: package_json.exists(),
        message: if package_json.exists() {
            "package.json found".to_string()
        } else {
            "package.json missing".to_string()
        },
    });

    let lock = cwd.join("kawkab.lock");
    checks.push(DoctorCheck {
        name: "lockfile_status".to_string(),
        ok: lock.exists(),
        message: if lock.exists() {
            "kawkab.lock present".to_string()
        } else {
            "kawkab.lock missing (run kawkab install)".to_string()
        },
    });

    checks.push(DoctorCheck {
        name: "workspace_discovery".to_string(),
        ok: true,
        message: format!(
            "discovered {} workspace package(s)",
            workspace.packages.len()
        ),
    });

    let cache = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("kawkab")
        .join("packages");
    let cache_ok = fs::create_dir_all(&cache).is_ok();
    checks.push(DoctorCheck {
        name: "cache_writable".to_string(),
        ok: cache_ok,
        message: format!("cache path: {}", cache.display()),
    });

    let passed = checks.iter().filter(|c| c.ok).count();
    let total = checks.len();
    DoctorReport {
        schema_version: "doctor-report.v1".to_string(),
        project_root: cwd.display().to_string(),
        checks,
        passed,
        total,
    }
}
