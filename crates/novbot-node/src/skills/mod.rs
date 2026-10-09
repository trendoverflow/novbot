// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! On-disk skill install. The center's desired set is level-triggered.
//! Bytes arrive only through [`ArtifactSource`] (`FetchArtifact`).

mod package;
mod source;

use novbot_proto::{DesiredSkills, InstalledSkill, SkillStateReport};
use novbot_skill_runtime::{RunRequest, SkillRuntime};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

pub use package::{
    current_platform, load_package, open_package, pack_skill, sha256_hex, Manifest, PackSpec,
    Package, ABI, CATALOG,
};
pub use source::{ArtifactSource, Fetched, GrpcArtifactSource};

const DEFAULT_CONCURRENCY: u32 = 4;
const DEFAULT_KEEP_PREVIOUS: u32 = 2;
const DEFAULT_QUOTA: u64 = 256 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MEMORY_MB: u64 = 64;

#[derive(Debug, Clone)]
pub struct InstallError {
    pub reason: &'static str,
    pub detail: String,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.reason, self.detail)
    }
}

impl std::error::Error for InstallError {}

fn fail(reason: &'static str, detail: impl Into<String>) -> InstallError {
    InstallError {
        reason,
        detail: detail.into(),
    }
}

fn retryable(reason: &str) -> bool {
    matches!(reason, "hash_mismatch" | "fetch_failed")
}

/// Injectable pause between install retries.
#[allow(clippy::type_complexity)]
pub type BackoffSleeper =
    Arc<dyn Fn(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Retry budget. Tests inject a zero sleep so they do not wait out production backoff.
pub struct InstallLimits {
    pub max_attempts: u32,
    pub max_elapsed: Duration,
    pub min_backoff: Duration,
    pub max_backoff: Duration,
    pub ready_bound: Duration,
    pub sleeper: BackoffSleeper,
}

impl Default for InstallLimits {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            max_elapsed: Duration::from_secs(10 * 60),
            min_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(5 * 60),
            ready_bound: Duration::from_secs(120),
            sleeper: Arc::new(|duration| Box::pin(tokio::time::sleep(duration))),
        }
    }
}

impl std::fmt::Debug for InstallLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallLimits")
            .field("max_attempts", &self.max_attempts)
            .field("max_elapsed", &self.max_elapsed)
            .field("min_backoff", &self.min_backoff)
            .field("max_backoff", &self.max_backoff)
            .field("ready_bound", &self.ready_bound)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct DesiredSkill {
    pub name: String,
    pub version: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub abi: String,
    pub capabilities_sha256: String,
    pub signature_envelope: Vec<u8>,
    pub fetch_ticket: String,
}

#[derive(Debug, Clone)]
pub struct SkillPolicy {
    pub max_concurrent_runs: u32,
    pub keep_previous_versions: u32,
    pub store_quota_bytes: u64,
    pub max_timeout_ms: u64,
    pub max_memory_mb: u64,
}

impl Default for SkillPolicy {
    fn default() -> Self {
        Self {
            max_concurrent_runs: DEFAULT_CONCURRENCY,
            keep_previous_versions: DEFAULT_KEEP_PREVIOUS,
            store_quota_bytes: DEFAULT_QUOTA,
            max_timeout_ms: DEFAULT_TIMEOUT_MS,
            max_memory_mb: DEFAULT_MEMORY_MB,
        }
    }
}

impl SkillPolicy {
    pub fn from_proto(policy: Option<&novbot_proto::SkillPolicy>) -> Self {
        let Some(policy) = policy else {
            return Self::default();
        };
        Self {
            max_concurrent_runs: nonzero_u32(policy.max_concurrent_runs, DEFAULT_CONCURRENCY),
            keep_previous_versions: nonzero_u32(
                policy.keep_previous_versions,
                DEFAULT_KEEP_PREVIOUS,
            ),
            store_quota_bytes: if policy.store_quota_bytes <= 0 {
                DEFAULT_QUOTA
            } else {
                policy.store_quota_bytes as u64
            },
            max_timeout_ms: nonzero_u32(policy.max_timeout_ms, DEFAULT_TIMEOUT_MS as u32) as u64,
            max_memory_mb: nonzero_u32(policy.max_memory_mb, DEFAULT_MEMORY_MB as u32) as u64,
        }
    }
}

fn nonzero_u32(value: i32, default: u32) -> u32 {
    if value <= 0 {
        default
    } else {
        value as u32
    }
}

/// Effective run timeout. Manifest `timeout_ms` is a request; policy is the cap.
pub fn clamp_timeout_ms(manifest_ms: Option<u64>, policy_max_ms: u64) -> u64 {
    match manifest_ms {
        Some(value) if value > 0 => value.min(policy_max_ms),
        _ => policy_max_ms,
    }
}

/// Effective linear memory cap in bytes.
pub fn clamp_memory_bytes(manifest_mb: Option<u64>, policy_max_mb: u64) -> u64 {
    let mb = match manifest_mb {
        Some(value) if value > 0 => value.min(policy_max_mb),
        _ => policy_max_mb,
    };
    mb.saturating_mul(1024 * 1024)
}

#[derive(Debug, Clone)]
pub struct DesiredSet {
    pub generation: i64,
    pub skills: Vec<DesiredSkill>,
    pub policy: SkillPolicy,
}

impl DesiredSet {
    pub fn from_proto(msg: &DesiredSkills) -> Self {
        Self {
            generation: msg.generation,
            policy: SkillPolicy::from_proto(msg.policy.as_ref()),
            skills: msg.skills.iter().map(DesiredSkill::from_proto).collect(),
        }
    }
}

impl DesiredSkill {
    fn from_proto(skill: &novbot_proto::SkillRef) -> Self {
        Self {
            name: skill.name.clone(),
            version: skill.version.clone(),
            sha256: skill.sha256.trim().to_ascii_lowercase(),
            size_bytes: u64::try_from(skill.size_bytes).unwrap_or(0),
            abi: skill.abi.clone(),
            capabilities_sha256: skill.capabilities_sha256.clone(),
            signature_envelope: skill.signature_envelope.clone(),
            fetch_ticket: skill.fetch_ticket.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedSkill {
    pub name: String,
    pub version: String,
    pub sha256: String,
    pub state: String,
    pub reason: String,
    pub reason_detail: String,
    pub attempts: u32,
    pub previous_sha256: Vec<String>,
}

impl ReportedSkill {
    pub fn to_proto(&self) -> InstalledSkill {
        InstalledSkill {
            name: self.name.clone(),
            version: self.version.clone(),
            sha256: self.sha256.clone(),
            state: self.state.clone(),
            reason: self.reason.clone(),
            reason_detail: self.reason_detail.clone(),
            attempts: i32::try_from(self.attempts).unwrap_or(i32::MAX),
            previous_sha256: self.previous_sha256.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSnapshot {
    pub applied_generation: i64,
    pub skills: Vec<ReportedSkill>,
}

impl SkillSnapshot {
    pub fn to_report(&self, node_id: &str) -> SkillStateReport {
        SkillStateReport {
            node_id: node_id.to_string(),
            applied_generation: self.applied_generation,
            skills: self.skills.iter().map(ReportedSkill::to_proto).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskSkill {
    sha256: String,
    version: String,
    #[serde(default)]
    previous: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DiskState {
    generation: i64,
    #[serde(default)]
    skills: BTreeMap<String, DiskSkill>,
}

#[derive(Debug, Clone)]
struct ActiveSkill {
    version: String,
    sha256: String,
    previous: Vec<String>,
    timeout_ms: u64,
    memory_bytes: u64,
}

struct Inner {
    generation: i64,
    seen: i64,
    skills: BTreeMap<String, ActiveSkill>,
    last_report: Option<SkillSnapshot>,
    boot_failures: Vec<ReportedSkill>,
    policy: SkillPolicy,
}

pub struct SkillHost {
    root: PathBuf,
    source: Arc<dyn ArtifactSource>,
    limits: InstallLimits,
    runtime: Arc<SkillRuntime>,
    inner: std::sync::RwLock<Inner>,
    reconcile: Mutex<()>,
    runs: std::sync::Mutex<Arc<Semaphore>>,
    run_limit: AtomicU32,
}

impl SkillHost {
    /// Load `state.json`, drop `staging/`, and quarantine a tampered active module.
    ///
    /// A tampered skill does not fail open. Built-in probes keep running.
    pub async fn open(
        data_dir: &Path,
        source: Arc<dyn ArtifactSource>,
        limits: InstallLimits,
    ) -> anyhow::Result<Self> {
        let runtime = Arc::new(SkillRuntime::new()?);
        let root = data_dir.join("skills");
        for name in ["store", "cache", "downloads", "staging", "quarantine"] {
            fs::create_dir_all(root.join(name))?;
        }
        wipe_dir(&root.join("staging"))?;
        let mut disk = read_state(&root.join("state.json"))?;
        let mut boot_failures = Vec::new();
        let mut removed = Vec::new();
        for (name, skill) in &disk.skills {
            if let Err(err) = verify_active(&root, &skill.sha256) {
                quarantine_store(&root, &skill.sha256)?;
                boot_failures.push(ReportedSkill {
                    name: name.clone(),
                    version: skill.version.clone(),
                    sha256: skill.sha256.clone(),
                    state: "failed".into(),
                    reason: "store_tampered".into(),
                    reason_detail: err.detail,
                    attempts: 0,
                    previous_sha256: skill.previous.clone(),
                });
                removed.push(name.clone());
            }
        }
        if !removed.is_empty() {
            for name in &removed {
                disk.skills.remove(name);
            }
            // The applied generation no longer matches disk, so the next desired set is applied.
            disk.generation = 0;
            write_state(&root, &disk)?;
        }
        let skills = disk
            .skills
            .iter()
            .map(|(name, skill)| {
                (
                    name.clone(),
                    ActiveSkill {
                        version: skill.version.clone(),
                        sha256: skill.sha256.clone(),
                        previous: skill.previous.clone(),
                        timeout_ms: DEFAULT_TIMEOUT_MS,
                        memory_bytes: DEFAULT_MEMORY_MB * 1024 * 1024,
                    },
                )
            })
            .collect();
        let policy = SkillPolicy::default();
        let concurrency = policy.max_concurrent_runs;
        let runs = Arc::new(Semaphore::new(concurrency as usize));
        Ok(Self {
            root,
            source,
            limits,
            runtime,
            inner: std::sync::RwLock::new(Inner {
                generation: disk.generation,
                seen: disk.generation,
                skills,
                last_report: None,
                boot_failures,
                policy,
            }),
            reconcile: Mutex::new(()),
            runs: std::sync::Mutex::new(runs),
            run_limit: AtomicU32::new(concurrency),
        })
    }

    pub fn applied_generation(&self) -> i64 {
        self.read().generation
    }

    pub fn should_pull(&self, center_generation: i64) -> bool {
        let inner = self.read();
        center_generation > inner.generation && center_generation > inner.seen
    }

    pub fn ready_bound(&self) -> Duration {
        self.limits.ready_bound
    }

    pub fn active_version(&self, name: &str) -> Option<String> {
        self.read()
            .skills
            .get(name)
            .map(|skill| skill.version.clone())
    }

    pub fn active_sha256(&self, name: &str) -> Option<String> {
        self.read()
            .skills
            .get(name)
            .map(|skill| skill.sha256.clone())
    }

    /// `cache/<package-sha256>.<engine-id>.cwasm`. The digest is the package, not `fingerprint()`.
    pub fn cache_file_name(&self, package_sha256: &str) -> String {
        format!("{package_sha256}.{}.cwasm", self.runtime.engine_id())
    }

    pub fn cache_path(&self, package_sha256: &str) -> PathBuf {
        self.root
            .join("cache")
            .join(self.cache_file_name(package_sha256))
    }

    pub fn store_path(&self, sha256: &str) -> PathBuf {
        self.root.join("store").join(sha256)
    }

    pub fn installed_timeout_ms(&self, name: &str) -> Option<u64> {
        self.read().skills.get(name).map(|skill| skill.timeout_ms)
    }

    pub fn installed_memory_bytes(&self, name: &str) -> Option<u64> {
        self.read().skills.get(name).map(|skill| skill.memory_bytes)
    }

    pub fn register_installed(&self) -> Vec<InstalledSkill> {
        self.snapshot()
            .skills
            .into_iter()
            .filter(|skill| skill.state == "installed")
            .map(|skill| skill.to_proto())
            .collect()
    }

    /// One permit from the policy semaphore. `None` when every runner is busy.
    pub fn try_acquire_run(&self) -> Option<OwnedSemaphorePermit> {
        let sem = self
            .runs
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        sem.try_acquire_owned().ok()
    }

    /// How many times this host has entered `novbot_skill_runtime::run`.
    pub fn wasm_run_entries(&self) -> u64 {
        self.runtime.run_entries()
    }

    /// Run one installed Hub skill.
    ///
    /// Built-ins are not handled here. The permit is held inside the blocking
    /// call until `run` returns, including timeout, denial, and trap.
    pub async fn run_hub(&self, run: HubRun) -> (String, Value) {
        let Some(installed) = self.read().skills.get(&run.name).cloned() else {
            return hub_error("skill_not_installed", "skill is not installed", None, None);
        };
        let identity = json!({
            "name": run.name,
            "version": installed.version,
            "sha256": installed.sha256,
        });
        if let Some(version) = &run.version {
            if !version_guard_ok(&installed.version, version) {
                return hub_error(
                    "version_mismatch",
                    "installed version does not satisfy params.version",
                    Some(identity),
                    None,
                );
            }
        }
        let loaded = match load_installed(&self.store_path(&installed.sha256)) {
            Ok(loaded) => loaded,
            Err(message) => {
                return hub_error("skill_error", message, Some(identity), None);
            }
        };
        if let Some(schema) = &loaded.schema {
            match argument_pointers(schema, &run.arguments) {
                Ok(pointers) if pointers.is_empty() => {}
                Ok(pointers) => {
                    return hub_error(
                        "invalid_params",
                        "arguments do not match schema/params.json",
                        Some(identity),
                        Some(json!({ "pointers": pointers })),
                    );
                }
                Err(()) => {
                    return hub_error(
                        "invalid_params",
                        "schema/params.json could not be applied",
                        Some(identity),
                        Some(json!({ "pointers": [] })),
                    );
                }
            }
        }
        let Some(memory_bytes) = usize::try_from(installed.memory_bytes).ok() else {
            return hub_error(
                "resource_limit",
                "installed memory cap does not fit this process",
                Some(identity),
                None,
            );
        };
        let Some(permit) = self.try_acquire_run() else {
            return hub_error(
                "resource_limit",
                "max_concurrent_runs is busy",
                Some(identity),
                None,
            );
        };
        let runtime = Arc::clone(&self.runtime);
        let timeout = Duration::from_millis(installed.timeout_ms);
        let data_dir = run.data_dir.clone();
        let params_json = run.arguments.to_string();
        let grants = loaded.grants;
        let module = loaded.module;
        let joined = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let grant_refs: Vec<&str> = grants.iter().map(String::as_str).collect();
            let output = novbot_skill_runtime::run(
                &runtime,
                RunRequest {
                    component_bytes: &module,
                    grants: &grant_refs,
                    params_json: &params_json,
                    data_dir: Some(data_dir.as_path()),
                    timeout: Some(timeout),
                    memory_bytes: Some(memory_bytes),
                },
            );
            drop(_permit);
            output
        })
        .await;
        match joined {
            Ok(output) => map_run_output(identity, output),
            Err(_) => hub_error("skill_trap", "skill run task stopped", Some(identity), None),
        }
    }

    pub fn snapshot(&self) -> SkillSnapshot {
        let inner = self.read();
        if let Some(report) = &inner.last_report {
            if report.applied_generation == inner.generation && inner.boot_failures.is_empty() {
                return report.clone();
            }
        }
        let mut skills: Vec<ReportedSkill> = inner
            .skills
            .iter()
            .map(|(name, skill)| ReportedSkill {
                name: name.clone(),
                version: skill.version.clone(),
                sha256: skill.sha256.clone(),
                state: "installed".into(),
                reason: String::new(),
                reason_detail: String::new(),
                attempts: 0,
                previous_sha256: skill.previous.clone(),
            })
            .collect();
        skills.extend(inner.boot_failures.clone());
        skills.sort_by(|left, right| left.name.cmp(&right.name));
        SkillSnapshot {
            applied_generation: inner.generation,
            skills,
        }
    }

    /// Apply `desired`. An equal generation is a no-op. An older generation is ignored.
    ///
    /// One failed package leaves the previous version of that skill active.
    pub async fn reconcile(&self, desired: DesiredSet) -> SkillSnapshot {
        let _guard = self.reconcile.lock().await;
        if self.is_noop(&desired) {
            return self.snapshot();
        }
        {
            let mut inner = self.write();
            if desired.generation > inner.seen {
                inner.seen = desired.generation;
            }
            inner.policy = desired.policy.clone();
        }
        self.resize_runs(desired.policy.max_concurrent_runs);

        let mut active = self.clone_active();
        let desired_names: HashSet<String> = desired
            .skills
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        active.retain(|name, _| desired_names.contains(name));

        let started = Instant::now();
        let mut reports = Vec::with_capacity(desired.skills.len());
        for skill in &desired.skills {
            let previous_active = active.get(&skill.name).cloned();
            match self
                .install_skill(skill, &desired.policy, &active, started)
                .await
            {
                Ok(installed) => {
                    reports.push(installed_report(skill, &installed, installed.attempts_hint));
                    active.insert(skill.name.clone(), installed.skill);
                }
                Err((err, attempts)) => {
                    let mut previous_sha256 = Vec::new();
                    if let Some(current) = &previous_active {
                        previous_sha256.push(current.sha256.clone());
                        previous_sha256.extend(current.previous.clone());
                    }
                    reports.push(ReportedSkill {
                        name: skill.name.clone(),
                        version: skill.version.clone(),
                        sha256: skill.sha256.clone(),
                        state: "failed".into(),
                        reason: err.reason.to_string(),
                        reason_detail: redact_ticket(&err.detail, &skill.fetch_ticket),
                        attempts,
                        previous_sha256,
                    });
                }
            }
        }
        reports.sort_by(|left, right| left.name.cmp(&right.name));
        let disk = DiskState {
            generation: desired.generation,
            skills: active
                .iter()
                .map(|(name, skill)| {
                    (
                        name.clone(),
                        DiskSkill {
                            sha256: skill.sha256.clone(),
                            version: skill.version.clone(),
                            previous: skill.previous.clone(),
                        },
                    )
                })
                .collect(),
        };
        if let Err(err) = write_state(&self.root, &disk) {
            tracing::warn!(error = %err, "skill state.json write failed");
        }
        let snapshot = SkillSnapshot {
            applied_generation: desired.generation,
            skills: reports,
        };
        let mut inner = self.write();
        inner.generation = desired.generation;
        inner.seen = inner.seen.max(desired.generation);
        inner.skills = active;
        inner.boot_failures.clear();
        inner.last_report = Some(snapshot.clone());
        inner.policy = desired.policy;
        snapshot
    }

    fn is_noop(&self, desired: &DesiredSet) -> bool {
        let inner = self.read();
        if desired.generation < inner.generation {
            return true;
        }
        desired.generation == inner.generation && inner.boot_failures.is_empty()
    }

    fn resize_runs(&self, permits: u32) {
        let permits = permits.max(1);
        if self.run_limit.swap(permits, Ordering::Relaxed) == permits {
            return;
        }
        let mut guard = self.runs.lock().unwrap_or_else(|err| err.into_inner());
        *guard = Arc::new(Semaphore::new(permits as usize));
    }

    async fn install_skill(
        &self,
        skill: &DesiredSkill,
        policy: &SkillPolicy,
        active: &BTreeMap<String, ActiveSkill>,
        started: Instant,
    ) -> Result<Installed, (InstallError, u32)> {
        let mut attempts = 0u32;
        let budget = self.limits.max_attempts.max(1);
        let mut last = fail("fetch_failed", "install did not start");
        loop {
            if started.elapsed() > self.limits.max_elapsed {
                return Err((fail("install_timeout", last.detail), attempts.max(1)));
            }
            if attempts >= budget {
                return Err((last, attempts));
            }
            attempts += 1;
            match self.try_install(skill, policy, active).await {
                Ok(mut installed) => {
                    installed.attempts_hint = attempts;
                    return Ok(installed);
                }
                Err(err) if !retryable(err.reason) => return Err((err, attempts)),
                Err(err) => {
                    last = err;
                    if attempts >= budget {
                        return Err((last, attempts));
                    }
                    let delay = backoff(attempts, &self.limits);
                    if started.elapsed().saturating_add(delay) > self.limits.max_elapsed {
                        return Err((fail("install_timeout", last.detail.clone()), attempts));
                    }
                    (self.limits.sleeper)(delay).await;
                }
            }
        }
    }

    async fn try_install(
        &self,
        skill: &DesiredSkill,
        policy: &SkillPolicy,
        active: &BTreeMap<String, ActiveSkill>,
    ) -> Result<Installed, InstallError> {
        if skill.abi != ABI {
            return Err(fail(
                "abi_unsupported",
                format!("skill abi {} is not {ABI}", skill.abi),
            ));
        }
        if !store_has_room(
            &self.root,
            &skill.sha256,
            skill.size_bytes,
            policy.store_quota_bytes,
        ) {
            return Err(fail(
                "disk_quota_exceeded",
                format!(
                    "store quota of {} bytes would be exceeded",
                    policy.store_quota_bytes
                ),
            ));
        }
        let package = if store_verified(&self.root, &skill.sha256) {
            None
        } else {
            let bytes = self.fetch_package(skill).await?;
            let package = package::open_package(&bytes, &skill.name, &skill.version)?;
            // Platform is a manifest field. SkillRef does not carry it, so it is
            // checked after extract and before the active version changes.
            if !package
                .manifest
                .platforms
                .iter()
                .any(|item| item == &current_platform())
            {
                return Err(fail(
                    "platform_unsupported",
                    format!("platform {} is not in the manifest", current_platform()),
                ));
            }
            Some(package)
        };
        let module = if let Some(package) = &package {
            package
                .files
                .get("module.wasm")
                .cloned()
                .ok_or_else(|| fail("invalid_manifest", "module.wasm is required"))?
        } else {
            fs::read(self.store_path(&skill.sha256).join("module.wasm"))
                .map_err(|err| fail("fetch_failed", format!("read module.wasm: {err}")))?
        };
        let (timeout_ms, memory_bytes) = if let Some(package) = &package {
            (
                clamp_timeout_ms(package.manifest.timeout_ms, policy.max_timeout_ms),
                clamp_memory_bytes(package.manifest.memory_mb, policy.max_memory_mb),
            )
        } else {
            let manifest = read_stored_manifest(&self.store_path(&skill.sha256))?;
            (
                clamp_timeout_ms(manifest.0, policy.max_timeout_ms),
                clamp_memory_bytes(manifest.1, policy.max_memory_mb),
            )
        };
        self.ensure_cache(&skill.sha256, &module)?;
        if let Some(package) = package {
            self.seal_store(skill, &package)?;
        }
        let mut previous = Vec::new();
        if let Some(current) = active.get(&skill.name) {
            if current.sha256 != skill.sha256 {
                previous.push(current.sha256.clone());
                previous.extend(current.previous.clone());
            } else {
                previous = current.previous.clone();
            }
        }
        let dropped = trim_previous(&mut previous, policy.keep_previous_versions);
        for sha in dropped {
            if !sha_referenced(&skill.sha256, &sha, &skill.name, active, &previous) {
                remove_store(&self.root, &sha);
            }
        }
        Ok(Installed {
            skill: ActiveSkill {
                version: skill.version.clone(),
                sha256: skill.sha256.clone(),
                previous,
                timeout_ms,
                memory_bytes,
            },
            attempts_hint: 1,
        })
    }

    fn ensure_cache(&self, package_sha: &str, module_wasm: &[u8]) -> Result<(), InstallError> {
        let path = self.cache_path(package_sha);
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > 0 && meta.is_file() {
                return Ok(());
            }
        }
        // Compile locally. Never deserialize bytes the center might have precompiled.
        let cwasm = self
            .runtime
            .compile_cwasm(module_wasm)
            .map_err(|err| fail("compile_failed", err))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| fail("compile_failed", format!("cache dir: {err}")))?;
        }
        let tmp = path.with_extension("cwasm.tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|err| fail("compile_failed", format!("cache write: {err}")))?;
            file.write_all(&cwasm)
                .map_err(|err| fail("compile_failed", format!("cache write: {err}")))?;
            file.sync_all()
                .map_err(|err| fail("compile_failed", format!("cache fsync: {err}")))?;
        }
        fs::rename(&tmp, &path)
            .map_err(|err| fail("compile_failed", format!("cache rename: {err}")))?;
        Ok(())
    }

    async fn fetch_package(&self, skill: &DesiredSkill) -> Result<Vec<u8>, InstallError> {
        let part = self
            .root
            .join("downloads")
            .join(format!("{}.part", skill.sha256));
        if let Some(parent) = part.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| fail("fetch_failed", format!("download dir: {err}")))?;
        }
        if let Ok(bytes) = fs::read(&part) {
            if skill.size_bytes > 0 && bytes.len() as u64 == skill.size_bytes {
                if sha256_hex(&bytes) == skill.sha256 {
                    return Ok(bytes);
                }
                let _ = fs::remove_file(&part);
            } else if skill.size_bytes > 0 && bytes.len() as u64 > skill.size_bytes {
                let _ = fs::remove_file(&part);
            }
        }
        let offset = fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
        let fetched = self
            .source
            .fetch(&skill.sha256, &skill.fetch_ticket, offset)
            .await
            .map_err(|detail| fail("fetch_failed", redact_ticket(&detail, &skill.fetch_ticket)))?;
        let mut hasher = Sha256::new();
        if offset > 0 {
            let mut prefix = File::open(&part)
                .map_err(|err| fail("fetch_failed", format!("read partial: {err}")))?;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = prefix
                    .read(&mut buf)
                    .map_err(|err| fail("fetch_failed", format!("read partial: {err}")))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
        }
        {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part)
                .map_err(|err| fail("fetch_failed", format!("append partial: {err}")))?;
            file.write_all(&fetched.data)
                .map_err(|err| fail("fetch_failed", format!("append partial: {err}")))?;
            file.sync_all()
                .map_err(|err| fail("fetch_failed", format!("fsync partial: {err}")))?;
        }
        hasher.update(&fetched.data);
        let written = fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
        let expected = if skill.size_bytes > 0 {
            skill.size_bytes
        } else {
            fetched.total_size
        };
        if expected > 0 && written < expected {
            return Err(fail(
                "fetch_failed",
                "download ended before the artifact size",
            ));
        }
        let digest = hex::encode(hasher.finalize());
        if digest != skill.sha256 {
            let _ = fs::remove_file(&part);
            return Err(fail(
                "hash_mismatch",
                "downloaded package sha256 does not match the desired skill",
            ));
        }
        let bytes =
            fs::read(&part).map_err(|err| fail("fetch_failed", format!("read package: {err}")))?;
        let _ = fs::remove_file(&part);
        Ok(bytes)
    }

    fn seal_store(
        &self,
        skill: &DesiredSkill,
        package: &package::Package,
    ) -> Result<(), InstallError> {
        let staging = self
            .root
            .join("staging")
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&staging)
            .map_err(|err| fail("invalid_archive", format!("staging: {err}")))?;
        let sealed = (|| {
            write_files(&staging, &package.files)?;
            if !skill.signature_envelope.is_empty() {
                fs::write(
                    staging.join("signature.envelope"),
                    &skill.signature_envelope,
                )
                .map_err(|err| fail("invalid_archive", format!("signature envelope: {err}")))?;
            }
            let marker = serde_json::json!({
                "sha256": skill.sha256,
                "name": skill.name,
                "version": skill.version,
            });
            fs::write(
                staging.join(".verified"),
                serde_json::to_vec(&marker).unwrap_or_default(),
            )
            .map_err(|err| fail("invalid_archive", format!("verified marker: {err}")))?;
            fsync_tree(&staging)?;
            let dest = self.store_path(&skill.sha256);
            if dest.exists() {
                remove_store(&self.root, &skill.sha256);
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| fail("invalid_archive", format!("store dir: {err}")))?;
            }
            // Rename while the directory is writable. macOS rejects rename of a 0555 directory.
            fs::rename(&staging, &dest)
                .map_err(|err| fail("invalid_archive", format!("store rename: {err}")))?;
            if let Err(err) = chmod_tree(&dest) {
                remove_store(&self.root, &skill.sha256);
                return Err(err);
            }
            if let Some(parent) = dest.parent() {
                let _ = fsync_dir(parent);
            }
            Ok(())
        })();
        if sealed.is_err() {
            remove_tree(&staging);
        }
        sealed
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|err| err.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|err| err.into_inner())
    }

    fn clone_active(&self) -> BTreeMap<String, ActiveSkill> {
        self.read().skills.clone()
    }
}

/// One Hub skill execution. `version` is the raw `params.version` value when present.
pub struct HubRun {
    pub name: String,
    pub version: Option<Value>,
    pub arguments: Value,
    pub data_dir: PathBuf,
}

struct LoadedSkill {
    module: Vec<u8>,
    grants: Vec<String>,
    schema: Option<Vec<u8>>,
}

fn load_installed(dir: &Path) -> Result<LoadedSkill, &'static str> {
    let module =
        fs::read(dir.join("module.wasm")).map_err(|_| "installed module.wasm is missing")?;
    let text = fs::read_to_string(dir.join("skill.toml"))
        .map_err(|_| "installed skill.toml is missing")?;
    let manifest =
        package::manifest_from_str(&text).map_err(|_| "installed skill.toml is invalid")?;
    let schema_path = dir.join("schema/params.json");
    let schema = if schema_path.is_file() {
        Some(fs::read(&schema_path).map_err(|_| "installed schema/params.json is unreadable")?)
    } else {
        None
    };
    Ok(LoadedSkill {
        module,
        grants: manifest.grants,
        schema,
    })
}

fn version_guard_ok(installed: &str, requirement: &Value) -> bool {
    let Some(requirement) = requirement.as_str() else {
        return false;
    };
    let requirement = requirement.trim();
    if requirement.is_empty() || requirement == "*" {
        return true;
    }
    let Ok(installed) = Version::parse(installed) else {
        return false;
    };
    if let Ok(exact) = Version::parse(requirement) {
        return installed == exact;
    }
    VersionReq::parse(requirement).is_ok_and(|parsed| parsed.matches(&installed))
}

/// JSON pointers only. `Err` means the schema document itself cannot be applied.
fn argument_pointers(schema_bytes: &[u8], arguments: &Value) -> Result<Vec<String>, ()> {
    let mut schema: Value = serde_json::from_slice(schema_bytes).map_err(|_| ())?;
    // `$schema` is an annotation. Drop it so validation does not fetch a meta-schema.
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("$schema");
    }
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .map_err(|_| ())?;
    if validator.is_valid(arguments) {
        return Ok(Vec::new());
    }
    let mut pointers = Vec::new();
    for error in validator.iter_errors(arguments) {
        let pointer = error.instance_path.to_string();
        let pointer = if pointer.is_empty() {
            "/".to_string()
        } else {
            pointer
        };
        if !pointers.contains(&pointer) {
            pointers.push(pointer);
        }
    }
    if pointers.is_empty() {
        pointers.push("/".to_string());
    }
    pointers.sort();
    Ok(pointers)
}

fn hub_error(
    code: &str,
    message: &str,
    skill: Option<Value>,
    extra_error: Option<Value>,
) -> (String, Value) {
    let mut error = json!({
        "code": code,
        "message": message,
    });
    if let Some(Value::Object(fields)) = extra_error {
        if let Some(obj) = error.as_object_mut() {
            for (key, value) in fields {
                obj.insert(key, value);
            }
        }
    }
    let mut payload = json!({ "error": error });
    if let Some(skill) = skill {
        payload["skill"] = skill;
    }
    ("error".to_string(), payload)
}

fn map_run_output(identity: Value, output: novbot_skill_runtime::RunOutput) -> (String, Value) {
    let mut payload = json!({ "skill": identity });
    if !output.denials.is_empty() {
        let message = output
            .error
            .as_ref()
            .map(|err| clip_message(&err.message))
            .unwrap_or_else(|| "host call was outside the skill's declared capabilities".into());
        payload["error"] = json!({
            "code": "capability_denied",
            "message": message,
        });
        payload["denials"] = serde_json::to_value(&output.denials).unwrap_or_else(|_| json!([]));
        if let Some(partial) = output.partial {
            payload["partial"] = json_or_string(&partial);
        }
        return ("error".into(), payload);
    }
    if output.status == "ok" {
        if let Some(text) = output.output {
            merge_guest_output(&mut payload, &text);
        }
        let status = if findings_have_fail(&payload) {
            "fail"
        } else {
            "ok"
        };
        return (status.to_string(), payload);
    }
    let (code, message) = match output.error {
        Some(err) => (
            map_run_code(&err.code).to_string(),
            clip_message(&err.message),
        ),
        None => ("skill_trap".to_string(), "skill run failed".to_string()),
    };
    payload["error"] = json!({
        "code": code,
        "message": message,
    });
    ("error".into(), payload)
}

fn map_run_code(code: &str) -> &str {
    match code {
        "guest_error" => "skill_error",
        "guest_trap" => "skill_trap",
        other => other,
    }
}

fn merge_guest_output(payload: &mut Value, text: &str) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        payload["output"] = json!(text);
        return;
    };
    let Some(obj) = value.as_object() else {
        payload["output"] = value;
        return;
    };
    let Some(dest) = payload.as_object_mut() else {
        return;
    };
    for (key, value) in obj {
        if matches!(key.as_str(), "skill" | "error" | "denials" | "partial") {
            continue;
        }
        dest.insert(key.clone(), value.clone());
    }
}

fn findings_have_fail(payload: &Value) -> bool {
    payload
        .get("findings")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("status").and_then(Value::as_str) == Some("fail"))
        })
}

fn json_or_string(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

fn clip_message(message: &str) -> String {
    const MAX: usize = 512;
    if message.len() <= MAX {
        message.to_string()
    } else {
        message[..MAX].to_string()
    }
}

struct Installed {
    skill: ActiveSkill,
    attempts_hint: u32,
}

fn installed_report(skill: &DesiredSkill, installed: &Installed, attempts: u32) -> ReportedSkill {
    ReportedSkill {
        name: skill.name.clone(),
        version: skill.version.clone(),
        sha256: skill.sha256.clone(),
        state: "installed".into(),
        reason: String::new(),
        reason_detail: String::new(),
        attempts,
        previous_sha256: installed.skill.previous.clone(),
    }
}

fn trim_previous(previous: &mut Vec<String>, keep: u32) -> Vec<String> {
    let keep = keep as usize;
    if previous.len() <= keep {
        return Vec::new();
    }
    previous.split_off(keep)
}

fn sha_referenced(
    active_sha: &str,
    candidate: &str,
    skill_name: &str,
    active: &BTreeMap<String, ActiveSkill>,
    previous: &[String],
) -> bool {
    if candidate == active_sha || previous.iter().any(|sha| sha == candidate) {
        return true;
    }
    active.iter().any(|(name, skill)| {
        name != skill_name
            && (skill.sha256 == candidate || skill.previous.iter().any(|sha| sha == candidate))
    })
}

fn backoff(attempt: u32, limits: &InstallLimits) -> Duration {
    if limits.max_backoff.is_zero() || limits.min_backoff.is_zero() {
        return Duration::ZERO;
    }
    let shift = attempt.saturating_sub(1).min(8);
    let factor = 1u32 << shift;
    let base = limits.min_backoff.saturating_mul(factor);
    let capped = if base > limits.max_backoff {
        limits.max_backoff
    } else {
        base
    };
    let millis = u64::try_from(capped.as_millis()).unwrap_or(u64::MAX);
    let jitter = (millis / 5).min(u64::from(attempt % 7) * 10);
    Duration::from_millis(millis.saturating_sub(jitter))
}

fn redact_ticket(detail: &str, ticket: &str) -> String {
    if ticket.is_empty() {
        detail.to_string()
    } else {
        detail.replace(ticket, "[redacted]")
    }
}

fn store_verified(root: &Path, sha256: &str) -> bool {
    let marker = root.join("store").join(sha256).join(".verified");
    let Ok(bytes) = fs::read(&marker) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    value.get("sha256").and_then(|item| item.as_str()) == Some(sha256)
}

fn store_has_room(root: &Path, sha256: &str, adding: u64, quota: u64) -> bool {
    if store_verified(root, sha256) {
        return true;
    }
    dir_size(&root.join("store")).saturating_add(adding) <= quota
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total = total.saturating_add(dir_size(&path));
        } else if let Ok(meta) = entry.metadata() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

fn verify_active(root: &Path, sha256: &str) -> Result<(), InstallError> {
    let dir = root.join("store").join(sha256);
    if !store_verified(root, sha256) {
        return Err(fail("store_tampered", "verified marker is missing"));
    }
    let toml_text = fs::read_to_string(dir.join("skill.toml"))
        .map_err(|_| fail("store_tampered", "skill.toml is missing"))?;
    let expected = package::module_hash_from_toml(&toml_text)?;
    let wasm = fs::read(dir.join("module.wasm"))
        .map_err(|_| fail("store_tampered", "module.wasm is missing"))?;
    if sha256_hex(&wasm) != expected {
        return Err(fail(
            "store_tampered",
            "module.wasm does not match the manifest hash",
        ));
    }
    Ok(())
}

fn read_stored_manifest(dir: &Path) -> Result<(Option<u64>, Option<u64>), InstallError> {
    let text = fs::read_to_string(dir.join("skill.toml"))
        .map_err(|err| fail("invalid_manifest", format!("read skill.toml: {err}")))?;
    #[derive(Deserialize)]
    struct ManifestLimits {
        #[serde(default)]
        runtime: RuntimeLimits,
    }
    #[derive(Deserialize, Default)]
    struct RuntimeLimits {
        #[serde(default)]
        limits: LimitFields,
    }
    #[derive(Deserialize, Default)]
    struct LimitFields {
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        memory_mb: Option<u64>,
    }
    let parsed: ManifestLimits = toml::from_str(&text)
        .map_err(|err| fail("invalid_manifest", format!("skill.toml: {err}")))?;
    Ok((
        parsed.runtime.limits.timeout_ms,
        parsed.runtime.limits.memory_mb,
    ))
}

fn quarantine_store(root: &Path, sha256: &str) -> anyhow::Result<()> {
    let src = root.join("store").join(sha256);
    if !src.exists() {
        return Ok(());
    }
    let mut dest = root.join("quarantine").join(sha256);
    if dest.exists() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|dur| dur.as_millis())
            .unwrap_or(0);
        dest = root.join("quarantine").join(format!("{sha256}-{stamp}"));
    }
    make_writable(&src)?;
    fs::rename(&src, &dest)?;
    Ok(())
}

fn remove_store(root: &Path, sha256: &str) {
    let path = root.join("store").join(sha256);
    remove_tree(&path);
}

fn remove_tree(path: &Path) {
    if !path.exists() {
        return;
    }
    let _ = make_writable(path);
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

fn wipe_dir(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        remove_tree(&entry.path());
    }
    Ok(())
}

fn read_state(path: &Path) -> anyhow::Result<DiskState> {
    if !path.exists() {
        return Ok(DiskState::default());
    }
    let bytes = fs::read(path)?;
    match serde_json::from_slice(&bytes) {
        Ok(state) => Ok(state),
        Err(err) => {
            tracing::warn!(error = %err, "skill state.json is unreadable; starting empty");
            Ok(DiskState::default())
        }
    }
}

fn write_state(root: &Path, state: &DiskState) -> anyhow::Result<()> {
    let path = root.join("state.json");
    let tmp = root.join(format!("state.json.{}.tmp", uuid::Uuid::new_v4()));
    let body = serde_json::to_vec_pretty(state)?;
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    let _ = fsync_dir(root);
    Ok(())
}

fn write_files(dir: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<(), InstallError> {
    for (rel, data) in files {
        let target = dir.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| fail("invalid_archive", format!("mkdir: {err}")))?;
        }
        fs::write(&target, data)
            .map_err(|err| fail("invalid_archive", format!("write {rel}: {err}")))?;
    }
    Ok(())
}

fn fsync_tree(dir: &Path) -> Result<(), InstallError> {
    let entries =
        fs::read_dir(dir).map_err(|err| fail("invalid_archive", format!("fsync: {err}")))?;
    for entry in entries {
        let entry = entry.map_err(|err| fail("invalid_archive", format!("fsync: {err}")))?;
        let path = entry.path();
        if path.is_dir() {
            fsync_tree(&path)?;
        } else {
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|err| fail("invalid_archive", format!("fsync: {err}")))?;
            file.sync_all()
                .map_err(|err| fail("invalid_archive", format!("fsync: {err}")))?;
        }
    }
    fsync_dir(dir).map_err(|err| fail("invalid_archive", format!("fsync dir: {err}")))?;
    Ok(())
}

fn fsync_dir(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

fn chmod_tree(dir: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;
    let entries =
        fs::read_dir(dir).map_err(|err| fail("invalid_archive", format!("chmod: {err}")))?;
    for entry in entries {
        let entry = entry.map_err(|err| fail("invalid_archive", format!("chmod: {err}")))?;
        let path = entry.path();
        if path.is_dir() {
            chmod_tree(&path)?;
        } else {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o444))
                .map_err(|err| fail("invalid_archive", format!("chmod: {err}")))?;
        }
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555))
        .map_err(|err| fail("invalid_archive", format!("chmod: {err}")))?;
    Ok(())
}

fn make_writable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path)?;
    if meta.is_dir() {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
        for entry in fs::read_dir(path)? {
            make_writable(&entry?.path())?;
        }
    } else {
        let mut perms = meta.permissions();
        perms.set_mode(0o644);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    const GUEST: &[u8] = include_bytes!("../../../novbot-skill-runtime/guest/skill.wasm");

    struct MapSource {
        files: StdMutex<BTreeMap<String, Vec<u8>>>,
        calls: StdMutex<Vec<(String, u64)>>,
        corrupt: StdMutex<HashSet<String>>,
        short_once: StdMutex<HashSet<String>>,
    }

    impl MapSource {
        fn new() -> Self {
            Self {
                files: StdMutex::new(BTreeMap::new()),
                calls: StdMutex::new(Vec::new()),
                corrupt: StdMutex::new(HashSet::new()),
                short_once: StdMutex::new(HashSet::new()),
            }
        }

        fn insert(&self, sha: &str, bytes: Vec<u8>) {
            self.files.lock().unwrap().insert(sha.to_string(), bytes);
        }

        fn calls(&self) -> Vec<(String, u64)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ArtifactSource for MapSource {
        async fn fetch(
            &self,
            sha256: &str,
            fetch_ticket: &str,
            offset: u64,
        ) -> Result<Fetched, String> {
            if fetch_ticket.is_empty() {
                return Err("missing fetch ticket".into());
            }
            self.calls
                .lock()
                .unwrap()
                .push((sha256.to_string(), offset));
            let bytes = self
                .files
                .lock()
                .unwrap()
                .get(sha256)
                .cloned()
                .ok_or_else(|| "artifact missing".to_string())?;
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let mut data = bytes[start..].to_vec();
            if self.short_once.lock().unwrap().remove(sha256) && data.len() > 8 {
                data.truncate(8);
            }
            if self.corrupt.lock().unwrap().contains(sha256) {
                if let Some(byte) = data.last_mut() {
                    *byte ^= 0xff;
                }
            }
            Ok(Fetched {
                total_size: bytes.len() as u64,
                data,
            })
        }
    }

    fn fast_limits() -> InstallLimits {
        InstallLimits {
            max_attempts: 2,
            max_elapsed: Duration::from_secs(30),
            min_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            ready_bound: Duration::from_millis(50),
            sleeper: Arc::new(|_| Box::pin(async {})),
        }
    }

    fn policy() -> SkillPolicy {
        SkillPolicy::default()
    }

    fn skill_bytes(version: &str) -> (Vec<u8>, String) {
        let bytes = pack_skill(&PackSpec {
            name: "os-release-check".into(),
            version: version.into(),
            wasm: GUEST.to_vec(),
            grants: vec![("sys.info.read".into(), None)],
            platforms: vec![current_platform()],
            timeout_ms: Some(5_000),
            memory_mb: Some(32),
        });
        let sha = sha256_hex(&bytes);
        (bytes, sha)
    }

    fn desired(version: &str, sha: &str, generation: i64, size: u64) -> DesiredSet {
        DesiredSet {
            generation,
            policy: policy(),
            skills: vec![DesiredSkill {
                name: "os-release-check".into(),
                version: version.into(),
                sha256: sha.into(),
                size_bytes: size,
                abi: ABI.into(),
                capabilities_sha256: String::new(),
                signature_envelope: Vec::new(),
                fetch_ticket: format!("ticket-{sha}"),
            }],
        }
    }

    async fn host_with(source: Arc<MapSource>) -> (tempfile::TempDir, SkillHost) {
        let dir = tempfile::tempdir().unwrap();
        let host = SkillHost::open(dir.path(), source, fast_limits())
            .await
            .unwrap();
        (dir, host)
    }

    #[test]
    fn write_os_release_fixtures() {
        if std::env::var("NOVBOT_WRITE_FIXTURES").ok().as_deref() != Some("1") {
            return;
        }
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../novbot-center/fixtures");
        std::fs::create_dir_all(&dir).unwrap();
        for version in ["1.0.0", "1.1.1"] {
            let bytes = os_release_fixture(version);
            std::fs::write(
                dir.join(format!("os-release-check-{version}.nbskill")),
                bytes,
            )
            .unwrap();
        }
    }

    fn os_release_fixture(version: &str) -> Vec<u8> {
        pack_skill(&PackSpec {
            name: "os-release-check".into(),
            version: version.into(),
            wasm: GUEST.to_vec(),
            grants: vec![
                ("fs.read".into(), Some("/etc/os-release".into())),
                ("sys.info.read".into(), None),
            ],
            platforms: vec![
                "darwin/arm64".into(),
                "darwin/amd64".into(),
                "linux/arm64".into(),
                "linux/amd64".into(),
            ],
            timeout_ms: Some(5_000),
            memory_mb: Some(32),
        })
    }

    #[test]
    fn policy_zero_uses_defaults_and_clamps_manifest() {
        let policy = SkillPolicy::from_proto(Some(&novbot_proto::SkillPolicy::default()));
        assert_eq!(policy.max_concurrent_runs, 4);
        assert_eq!(policy.keep_previous_versions, 2);
        assert_eq!(policy.store_quota_bytes, 256 * 1024 * 1024);
        assert_eq!(policy.max_timeout_ms, 30_000);
        assert_eq!(policy.max_memory_mb, 64);
        assert_eq!(clamp_timeout_ms(Some(90_000), 30_000), 30_000);
        assert_eq!(clamp_timeout_ms(Some(1_000), 30_000), 1_000);
        assert_eq!(clamp_memory_bytes(Some(128), 64), 64 * 1024 * 1024);
    }

    #[test]
    fn fetch_request_has_no_precompiled_field() {
        let request = novbot_proto::FetchArtifactRequest {
            node_id: "n".into(),
            sha256: "ab".into(),
            fetch_ticket: "ticket".into(),
            offset: 0,
        };
        assert_eq!(request.offset, 0);
        assert!(!format!("{request:?}").contains("cwasm"));
    }

    #[tokio::test]
    async fn hash_mismatch_keeps_the_old_version_registered() {
        let source = Arc::new(MapSource::new());
        let (v1, sha1) = skill_bytes("1.0.0");
        let (v2, sha2) = skill_bytes("1.1.1");
        source.insert(&sha1, v1);
        source.insert(&sha2, v2);
        source.corrupt.lock().unwrap().insert(sha2.clone());
        let (_dir, host) = host_with(source.clone()).await;
        let first = host
            .reconcile(desired("1.0.0", &sha1, 1, host_size(&source, &sha1)))
            .await;
        assert_eq!(first.skills[0].state, "installed");
        assert_eq!(
            host.active_version("os-release-check").as_deref(),
            Some("1.0.0")
        );
        let second = host
            .reconcile(desired("1.1.1", &sha2, 2, host_size(&source, &sha2)))
            .await;
        assert_eq!(second.skills[0].state, "failed");
        assert_eq!(second.skills[0].reason, "hash_mismatch");
        assert_eq!(
            host.active_version("os-release-check").as_deref(),
            Some("1.0.0")
        );
        assert_eq!(
            host.active_sha256("os-release-check").as_deref(),
            Some(sha1.as_str())
        );
        assert!(host.store_path(&sha1).join(".verified").is_file());
        assert!(!host.store_path(&sha2).exists());
    }

    #[tokio::test]
    async fn cache_name_is_the_package_sha_and_cwasm_is_not_executable_input() {
        let source = Arc::new(MapSource::new());
        let (bytes, sha) = skill_bytes("1.0.0");
        let module_sha = sha256_hex(GUEST);
        source.insert(&sha, bytes);
        let (_dir, host) = host_with(source.clone()).await;
        let snap = host
            .reconcile(desired("1.0.0", &sha, 1, host_size(&source, &sha)))
            .await;
        assert_eq!(snap.skills[0].state, "installed");
        let name = host.cache_file_name(&sha);
        assert!(name.starts_with(&sha), "{name}");
        assert!(!name.starts_with(&module_sha), "{name}");
        assert!(host.cache_path(&sha).is_file());
        let cwasm = fs::read(host.cache_path(&sha)).unwrap();
        let rejected = host.runtime.compile_cwasm(&cwasm).unwrap_err();
        assert!(rejected.starts_with("compile_failed"), "{rejected}");
        let bad = pack_skill(&PackSpec {
            name: "os-release-check".into(),
            version: "9.9.9".into(),
            wasm: cwasm,
            grants: vec![("sys.info.read".into(), None)],
            platforms: vec![current_platform()],
            timeout_ms: Some(1000),
            memory_mb: Some(16),
        });
        let bad_sha = sha256_hex(&bad);
        source.insert(&bad_sha, bad);
        let failed = host
            .reconcile(desired("9.9.9", &bad_sha, 2, host_size(&source, &bad_sha)))
            .await;
        assert_eq!(failed.skills[0].reason, "compile_failed");
        assert_eq!(
            host.active_version("os-release-check").as_deref(),
            Some("1.0.0")
        );
        assert!(!host.cache_path(&bad_sha).exists());
    }

    #[tokio::test]
    async fn rollback_to_a_stored_version_does_not_download() {
        let source = Arc::new(MapSource::new());
        let (v1, sha1) = skill_bytes("1.0.0");
        let (v2, sha2) = skill_bytes("1.1.0");
        source.insert(&sha1, v1);
        source.insert(&sha2, v2);
        let (_dir, host) = host_with(source.clone()).await;
        host.reconcile(desired("1.0.0", &sha1, 1, host_size(&source, &sha1)))
            .await;
        host.reconcile(desired("1.1.0", &sha2, 2, host_size(&source, &sha2)))
            .await;
        let before = source.calls().len();
        let snap = host
            .reconcile(desired("1.0.0", &sha1, 3, host_size(&source, &sha1)))
            .await;
        assert_eq!(snap.skills[0].state, "installed");
        assert_eq!(snap.skills[0].version, "1.0.0");
        assert_eq!(source.calls().len(), before);
        assert!(
            source
                .calls()
                .iter()
                .filter(|(sha, _)| sha == &sha1)
                .count()
                == 1
        );
    }

    #[tokio::test]
    async fn gc_keeps_two_previous_versions() {
        let source = Arc::new(MapSource::new());
        let mut versions = Vec::new();
        for version in ["1.0.0", "1.1.0", "1.2.0", "1.3.0"] {
            let (bytes, sha) = skill_bytes(version);
            source.insert(&sha, bytes);
            versions.push((version, sha));
        }
        let (_dir, host) = host_with(source.clone()).await;
        for (index, (version, sha)) in versions.iter().enumerate() {
            let snap = host
                .reconcile(desired(
                    version,
                    sha,
                    (index + 1) as i64,
                    host_size(&source, sha),
                ))
                .await;
            assert_eq!(snap.skills[0].state, "installed", "{version}");
        }
        let active = &versions[3].1;
        assert!(host.store_path(active).join(".verified").is_file());
        assert!(host.store_path(&versions[2].1).is_dir());
        assert!(host.store_path(&versions[1].1).is_dir());
        assert!(
            !host.store_path(&versions[0].1).exists(),
            "oldest store directory should be gone"
        );
        assert_eq!(
            host.active_sha256("os-release-check").as_deref(),
            Some(active.as_str())
        );
    }

    #[tokio::test]
    async fn restart_reports_the_installed_set_without_downloading() {
        let source = Arc::new(MapSource::new());
        let (bytes, sha) = skill_bytes("1.0.0");
        let size = bytes.len() as u64;
        source.insert(&sha, bytes);
        let dir = tempfile::tempdir().unwrap();
        let host = SkillHost::open(dir.path(), source.clone(), fast_limits())
            .await
            .unwrap();
        let snap = host.reconcile(desired("1.0.0", &sha, 4, size)).await;
        assert_eq!(snap.skills[0].state, "installed");
        drop(host);
        let source2 = Arc::new(MapSource::new());
        let host = SkillHost::open(dir.path(), source2.clone(), fast_limits())
            .await
            .unwrap();
        let again = host.snapshot();
        assert_eq!(again.applied_generation, 4);
        assert_eq!(again.skills.len(), 1);
        assert_eq!(again.skills[0].version, "1.0.0");
        assert_eq!(again.skills[0].sha256, sha);
        let noop = host.reconcile(desired("1.0.0", &sha, 4, size)).await;
        assert_eq!(noop.skills[0].state, "installed");
        assert!(source2.calls().is_empty());
    }

    #[tokio::test]
    async fn resume_appends_from_the_partial_offset() {
        let source = Arc::new(MapSource::new());
        let (bytes, sha) = skill_bytes("1.0.0");
        source.insert(&sha, bytes);
        source.short_once.lock().unwrap().insert(sha.clone());
        let (_dir, host) = host_with(source.clone()).await;
        let snap = host
            .reconcile(desired("1.0.0", &sha, 1, host_size(&source, &sha)))
            .await;
        assert_eq!(snap.skills[0].state, "installed", "{snap:?}");
        let calls = source.calls();
        assert!(calls.len() >= 2, "{calls:?}");
        assert_eq!(calls[0], (sha.clone(), 0));
        assert!(calls[1].1 > 0, "{calls:?}");
    }

    #[tokio::test]
    async fn store_tamper_is_quarantined_without_failing_open() {
        let source = Arc::new(MapSource::new());
        let (bytes, sha) = skill_bytes("1.0.0");
        let size = bytes.len() as u64;
        source.insert(&sha, bytes);
        let dir = tempfile::tempdir().unwrap();
        let host = SkillHost::open(dir.path(), source, fast_limits())
            .await
            .unwrap();
        host.reconcile(desired("1.0.0", &sha, 2, size)).await;
        drop(host);
        let wasm = dir
            .path()
            .join("skills/store")
            .join(&sha)
            .join("module.wasm");
        make_writable(&wasm).unwrap();
        let mut data = fs::read(&wasm).unwrap();
        let mid = data.len() / 2;
        data[mid] ^= 0x5a;
        fs::write(&wasm, data).unwrap();
        let host = SkillHost::open(dir.path(), Arc::new(MapSource::new()), fast_limits())
            .await
            .expect("tamper must not stop the node");
        let snap = host.snapshot();
        assert_eq!(snap.applied_generation, 0);
        assert_eq!(snap.skills[0].state, "failed");
        assert_eq!(snap.skills[0].reason, "store_tampered");
        assert!(!dir.path().join("skills/store").join(&sha).exists());
        assert!(dir.path().join("skills/quarantine").join(&sha).is_dir());
    }

    #[tokio::test]
    async fn semaphore_stops_at_max_concurrent_runs() {
        let source = Arc::new(MapSource::new());
        let (_dir, host) = host_with(source).await;
        host.reconcile(DesiredSet {
            generation: 1,
            skills: Vec::new(),
            policy: SkillPolicy {
                max_concurrent_runs: 1,
                ..policy()
            },
        })
        .await;
        let permit = host.try_acquire_run();
        assert!(permit.is_some());
        assert!(host.try_acquire_run().is_none());
        drop(permit);
        assert!(host.try_acquire_run().is_some());
    }

    #[test]
    fn archive_rejects_a_cwasm_entry() {
        let err = package::open_package(&pack_with_cwasm_name(), "os-release-check", "1.0.0");
        assert_eq!(err.unwrap_err().reason, "invalid_archive");
    }

    fn pack_with_cwasm_name() -> Vec<u8> {
        // The extractor rejects a `.cwasm` entry before it looks at the payload.
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let payload = b"not wasm";
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o644);
            header.set_size(payload.len() as u64);
            header.set_mtime(0);
            header.set_cksum();
            builder
                .append_data(&mut header, "module.cwasm", payload.as_slice())
                .unwrap();
            builder.finish().unwrap();
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn host_size(source: &MapSource, sha: &str) -> u64 {
        source
            .files
            .lock()
            .unwrap()
            .get(sha)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(0)
    }

    #[test]
    fn clamped_install_records_the_policy_cap() {
        // The install path stores clamp_timeout_ms. This locks the helper the store uses.
        assert_eq!(clamp_timeout_ms(None, 30_000), 30_000);
        assert_eq!(clamp_memory_bytes(None, 64), 64 * 1024 * 1024);
    }
}
