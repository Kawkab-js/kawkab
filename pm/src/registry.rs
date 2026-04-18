use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::Context;
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use semver::{Version, VersionReq};
use serde::Deserialize;
use tar::Archive;

#[derive(Clone)]
pub struct RegistryClient {
    client: Client,
    base_url: String,
    cache_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PackageTarball {
    pub name: String,
    pub version: String,
    pub resolved_url: String,
    pub integrity: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct NpmManifest {
    #[serde(default)]
    versions: serde_json::Map<String, serde_json::Value>,
    #[serde(rename = "dist-tags")]
    dist_tags: DistTags,
}

#[derive(Debug, Deserialize)]
struct DistTags {
    latest: String,
}

#[derive(Debug, Deserialize)]
struct VersionDoc {
    #[serde(default)]
    dependencies: std::collections::BTreeMap<String, String>,
    #[serde(default, rename = "peerDependencies")]
    peer_dependencies: std::collections::BTreeMap<String, String>,
    dist: DistDoc,
}

#[derive(Debug, Deserialize)]
struct DistDoc {
    tarball: String,
    #[serde(default)]
    integrity: String,
}

impl RegistryClient {
    pub fn new_default() -> anyhow::Result<Self> {
        let client = Client::builder().build()?;
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| std::env::temp_dir())
            .join("kawkab")
            .join("packages");
        fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            client,
            base_url: "https://registry.npmjs.org".to_string(),
            cache_dir,
        })
    }

    pub fn latest_version(&self, package: &str) -> anyhow::Result<Version> {
        let manifest = self.fetch_manifest(package)?;
        Version::parse(&manifest.dist_tags.latest)
            .with_context(|| format!("invalid latest semver for {package}"))
    }

    pub fn pick_version(&self, package: &str, req: &str) -> anyhow::Result<Version> {
        let manifest = self.fetch_manifest(package)?;
        let mut versions = manifest
            .versions
            .keys()
            .filter_map(|v| Version::parse(v).ok())
            .collect::<Vec<_>>();
        versions.sort();
        versions.reverse();
        for v in versions {
            if matches_version_req(req, &v) {
                return Ok(v);
            }
        }
        anyhow::bail!("no version matched {package}@{req}");
    }

    pub fn fetch_version_doc(
        &self,
        package: &str,
        version: &Version,
    ) -> anyhow::Result<(
        std::collections::BTreeMap<String, String>,
        std::collections::BTreeMap<String, String>,
        String,
        String,
    )> {
        let manifest = self.fetch_manifest(package)?;
        let key = version.to_string();
        let raw = manifest
            .versions
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("version metadata not found: {package}@{key}"))?;
        let doc: VersionDoc = serde_json::from_value(raw.clone())
            .with_context(|| format!("invalid registry version doc for {package}@{key}"))?;
        Ok((
            doc.dependencies,
            doc.peer_dependencies,
            doc.dist.tarball,
            doc.dist.integrity,
        ))
    }

    pub fn fetch_tarball(
        &self,
        package: &str,
        version: &Version,
    ) -> anyhow::Result<PackageTarball> {
        let cache_hit = self.read_cached_tarball(package, version)?;
        if let Some(cached) = cache_hit {
            return Ok(cached);
        }

        let (_, _, tarball_url, integrity) = self.fetch_version_doc(package, version)?;
        let bytes = self
            .client
            .get(&tarball_url)
            .send()
            .with_context(|| format!("failed to download tarball {tarball_url}"))?
            .error_for_status()
            .with_context(|| format!("registry returned error for {tarball_url}"))?
            .bytes()?
            .to_vec();

        let resolved = PackageTarball {
            name: package.to_string(),
            version: version.to_string(),
            resolved_url: tarball_url,
            integrity: normalize_integrity(&integrity, &bytes),
            bytes,
        };
        self.write_cached_tarball(&resolved)?;
        Ok(resolved)
    }

    pub fn extract_tarball_to(
        &self,
        tarball: &PackageTarball,
        target: &Path,
    ) -> anyhow::Result<()> {
        fs::create_dir_all(target)?;
        let mut archive = Archive::new(GzDecoder::new(Cursor::new(&tarball.bytes)));
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;
            let stripped = strip_package_prefix(path.as_ref());
            if stripped.as_os_str().is_empty() {
                continue;
            }
            let out = target.join(stripped);
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            entry.unpack(out)?;
        }
        Ok(())
    }

    fn fetch_manifest(&self, package: &str) -> anyhow::Result<NpmManifest> {
        let encoded = package.replace('/', "%2f");
        let url = format!("{}/{}", self.base_url, encoded);
        let body = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("failed to fetch metadata for {package}"))?
            .error_for_status()
            .with_context(|| format!("registry returned error for {package}"))?
            .text()?;
        let parsed = serde_json::from_str::<NpmManifest>(&body)
            .with_context(|| format!("invalid manifest payload for {package}"))?;
        Ok(parsed)
    }

    fn cache_file(&self, package: &str, version: &Version) -> PathBuf {
        self.cache_dir
            .join(package.replace('/', "__"))
            .with_extension(format!("{}.tgz", version))
    }

    fn read_cached_tarball(
        &self,
        package: &str,
        version: &Version,
    ) -> anyhow::Result<Option<PackageTarball>> {
        let file = self.cache_file(package, version);
        if !file.exists() {
            return Ok(None);
        }
        let bytes =
            fs::read(&file).with_context(|| format!("failed to read {}", file.display()))?;
        Ok(Some(PackageTarball {
            name: package.to_string(),
            version: version.to_string(),
            resolved_url: file.to_string_lossy().to_string(),
            integrity: normalize_integrity("", &bytes),
            bytes,
        }))
    }

    fn write_cached_tarball(&self, tarball: &PackageTarball) -> anyhow::Result<()> {
        let file = self.cache_file(&tarball.name, &Version::parse(&tarball.version)?);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&file, &tarball.bytes)?;
        Ok(())
    }
}

fn normalize_integrity(registry_value: &str, bytes: &[u8]) -> String {
    if !registry_value.trim().is_empty() {
        registry_value.to_string()
    } else {
        let digest = blake3::hash(bytes);
        format!("blake3-{}", digest.to_hex())
    }
}

fn strip_package_prefix(path: &Path) -> PathBuf {
    let mut it = path.components();
    let first = it.next();
    match first {
        Some(c) if c.as_os_str() == "package" => it.as_path().to_path_buf(),
        _ => path.to_path_buf(),
    }
}

pub fn matches_version_req(req: &str, version: &Version) -> bool {
    let req = req.trim();
    if req.is_empty() || req == "*" {
        return true;
    }
    if req.eq_ignore_ascii_case("latest") {
        return true;
    }
    if req.starts_with("workspace:") {
        return true;
    }
    if req.contains("||") {
        return req
            .split("||")
            .map(str::trim)
            .any(|part| matches_version_req(part, version));
    }
    if let Ok(vreq) = VersionReq::parse(req) {
        return vreq.matches(version);
    }
    if let Some(v) = req.strip_prefix('^') {
        if let Ok(base) = Version::parse(v) {
            return version.major == base.major
                && (version.minor > base.minor
                    || (version.minor == base.minor && version.patch >= base.patch));
        }
    }
    if let Some(v) = req.strip_prefix('~') {
        if let Ok(base) = Version::parse(v) {
            return version.major == base.major
                && version.minor == base.minor
                && version.patch >= base.patch;
        }
    }
    if let Ok(exact) = Version::parse(req) {
        return version == &exact;
    }
    false
}
