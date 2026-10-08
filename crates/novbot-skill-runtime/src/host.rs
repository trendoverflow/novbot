// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Component execution and the SH-3 host functions.

use crate::deny::Denylist;
use crate::grant::{self, CheckTarget, DenialReason, GrantSet};
use crate::path::{self, display_path, Resolved};
use crate::wasi_deny;
use crate::{Denial, RunOutput, DENIAL_LIMIT, MAX_READ_BYTES};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::Result;
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

const STDOUT_CAP: usize = 64 * 1024;
const LOG_LIMIT: usize = 32;
const LOG_BYTES: usize = 1024;
/// Epoch tick. A finite run deadline is this long, times the tick count.
const EPOCH_MILLIS: u64 = 10;
/// Ticks past the current epoch when the caller sets no deadline.
/// `u64::MAX` wraps once the engine epoch is non-zero (`current + delta`).
const OPEN_DEADLINE_TICKS: u64 = 1 << 62;

const SKILL_ABI: &str = "novbot:skill@1";

pub struct SkillRuntime {
    engine: Engine,
    cache: Mutex<Option<(u64, Component)>>,
}

pub struct RunRequest<'a> {
    pub component_bytes: &'a [u8],
    pub grants: &'a [&'a str],
    pub params_json: &'a str,
    pub data_dir: Option<&'a Path>,
    /// `None` leaves the epoch deadline open. A value is clamped to at least one tick.
    pub timeout: Option<Duration>,
    /// `None` does not install a [`StoreLimits`] memory cap.
    pub memory_bytes: Option<usize>,
}

pub(crate) struct RunCtx {
    table: ResourceTable,
    wasi: WasiCtx,
    grants: GrantSet,
    denylist: Denylist,
    started: Instant,
    denials: Vec<Denial>,
    logs: Vec<(u8, String)>,
    limits: StoreLimits,
}

impl SkillRuntime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        Ok(Self {
            engine,
            cache: Mutex::new(None),
        })
    }

    /// Filesystem-safe id: wasmtime version, target, and the engine's precompile hash.
    ///
    /// The hash covers CPU features. It is not a package digest.
    pub fn engine_id(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.engine
            .precompile_compatibility_hash()
            .hash(&mut hasher);
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        format!("wt49.0.2-{os}-{arch}-{:016x}", hasher.finish())
    }

    /// Compile `module.wasm` on this engine and return wasmtime serialized code.
    ///
    /// Precompiled bytes are rejected. Callers must not [`Component::deserialize`]
    /// a payload that arrived from the center.
    pub fn compile_cwasm(&self, module_wasm: &[u8]) -> std::result::Result<Vec<u8>, String> {
        if module_wasm.is_empty() || !module_wasm.starts_with(b"\0asm") {
            return Err("compile_failed: module.wasm is not a wasm component".into());
        }
        let component = Component::new(&self.engine, module_wasm)
            .map_err(|err| format!("compile_failed: {err}"))?;
        for (name, _) in component.component_type().imports(&self.engine) {
            if !import_allowed(name) {
                return Err(format!(
                    "compile_failed: import {name} is outside {SKILL_ABI}"
                ));
            }
        }
        component
            .serialize()
            .map_err(|err| format!("compile_failed: {err}"))
    }

    fn component(&self, bytes: &[u8]) -> anyhow::Result<Component> {
        let fingerprint = fingerprint(bytes);
        let mut cache = self.cache.lock().unwrap_or_else(|err| err.into_inner());
        if let Some((cached, component)) = cache.as_ref() {
            if *cached == fingerprint {
                return Ok(component.clone());
            }
        }
        let component = Component::new(&self.engine, bytes)
            .map_err(|err| anyhow::anyhow!("component bytes are not a wasm component: {err}"))?;
        *cache = Some((fingerprint, component.clone()));
        Ok(component)
    }
}

pub fn run(runtime: &SkillRuntime, request: RunRequest<'_>) -> RunOutput {
    let grants = match GrantSet::parse(request.grants) {
        Ok(grants) => grants,
        Err(err) => {
            return RunOutput::error("capability_unsupported", err.to_string(), Vec::new(), None);
        }
    };
    let component = match runtime.component(request.component_bytes) {
        Ok(component) => component,
        Err(err) => {
            return RunOutput::error("invalid_component", err.to_string(), Vec::new(), None);
        }
    };
    let invoke_result = if request.timeout.is_some() {
        with_epoch_ticks(&runtime.engine, || {
            invoke(runtime, &component, grants, &request)
        })
    } else {
        invoke(runtime, &component, grants, &request)
    };
    match invoke_result {
        Ok(output) => output,
        Err(err) => RunOutput::error("runtime_error", err.to_string(), Vec::new(), None),
    }
}

fn with_epoch_ticks<T>(engine: &Engine, body: impl FnOnce() -> T) -> T {
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let engine = engine.clone();
    let ticker = std::thread::Builder::new()
        .name("skill-epoch".into())
        .spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(EPOCH_MILLIS));
                engine.increment_epoch();
            }
        })
        .ok();
    let result = body();
    stop.store(true, Ordering::Relaxed);
    if let Some(ticker) = ticker {
        let _ = ticker.join();
    }
    result
}

fn import_allowed(name: &str) -> bool {
    name.starts_with("novbot:skill/") || name.starts_with("wasi:")
}

fn deadline_ticks(timeout: Option<Duration>) -> u64 {
    let Some(timeout) = timeout else {
        return OPEN_DEADLINE_TICKS;
    };
    let millis = timeout.as_millis().max(1);
    let ticks = millis.div_ceil(u128::from(EPOCH_MILLIS));
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
}

fn invoke(
    runtime: &SkillRuntime,
    component: &Component,
    grants: GrantSet,
    request: &RunRequest<'_>,
) -> anyhow::Result<RunOutput> {
    let mut linker = Linker::new(&runtime.engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    crate::Skill::add_to_linker::<_, HasSelf<_>>(&mut linker, |ctx| ctx)?;
    wasi_deny::install(&mut linker, &runtime.engine, component)?;

    let stdout = MemoryOutputPipe::new(STDOUT_CAP);
    let stderr = MemoryOutputPipe::new(STDOUT_CAP);
    let mut builder = WasiCtxBuilder::new();
    builder.stdout(stdout);
    builder.stderr(stderr);
    let ctx = RunCtx {
        table: ResourceTable::new(),
        wasi: builder.build(),
        grants,
        denylist: Denylist::new(request.data_dir),
        started: Instant::now(),
        denials: Vec::new(),
        logs: Vec::new(),
        limits: memory_limits(request.memory_bytes),
    };
    let mut store = Store::new(&runtime.engine, ctx);
    if request.memory_bytes.is_some() {
        store.limiter(|ctx| &mut ctx.limits);
    }
    store.set_epoch_deadline(deadline_ticks(request.timeout));

    let instance = match crate::Skill::instantiate(&mut store, component, &linker) {
        Ok(instance) => instance,
        Err(err) => {
            return Ok(RunOutput::error(
                trap_code(&err),
                err.to_string(),
                Vec::new(),
                None,
            ));
        }
    };
    let guest = instance.call_run(&mut store, request.params_json);
    Ok(finish(&mut store, guest))
}

fn finish(store: &mut Store<RunCtx>, guest: Result<Result<String, String>>) -> RunOutput {
    let ctx = store.data_mut();
    let _kept_logs = ctx.logs.len();
    let denials = std::mem::take(&mut ctx.denials);
    if !denials.is_empty() {
        let partial = match guest {
            Ok(Ok(text)) => Some(text),
            _ => None,
        };
        return RunOutput::error(
            "capability_denied",
            denial_message(denials.len()),
            denials,
            partial,
        );
    }
    match guest {
        Ok(Ok(output)) => RunOutput::ok(output),
        Ok(Err(message)) => RunOutput::error("guest_error", message, Vec::new(), None),
        Err(err) => {
            let code = trap_code(&err);
            RunOutput::error(code, err.to_string(), Vec::new(), None)
        }
    }
}

fn memory_limits(memory_bytes: Option<usize>) -> StoreLimits {
    match memory_bytes {
        Some(limit) => StoreLimitsBuilder::new()
            .memory_size(limit)
            .trap_on_grow_failure(true)
            .build(),
        None => StoreLimits::default(),
    }
}

fn trap_code(err: &wasmtime::Error) -> &'static str {
    let text = format!("{err:#}").to_ascii_lowercase();
    if text.contains("epoch")
        || text.contains("interrupt")
        || err
            .downcast_ref::<wasmtime::Trap>()
            .is_some_and(|trap| matches!(trap, wasmtime::Trap::Interrupt))
    {
        "exec_timeout"
    } else if text.contains("memory") || text.contains("resource") {
        "resource_limit"
    } else {
        "guest_trap"
    }
}

fn denial_message(count: usize) -> String {
    if count == 1 {
        "1 host call was outside the skill's declared capabilities".to_string()
    } else {
        format!("{count} host calls were outside the skill's declared capabilities")
    }
}

impl WasiView for RunCtx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl RunCtx {
    pub(crate) fn guard_limit(&self) -> Result<()> {
        if self.denials.len() >= DENIAL_LIMIT {
            return Err(wasmtime::Error::msg("capability denial limit reached"));
        }
        Ok(())
    }

    pub(crate) fn deny_wasi(&mut self, capability: &str, target: String) -> Result<()> {
        self.guard_limit()?;
        let reason = grant::check(&self.grants, capability, CheckTarget::None, &self.denylist)
            .err()
            .unwrap_or(DenialReason::UndeclaredCapability);
        self.push_denial(capability.to_string(), target, reason);
        Ok(())
    }

    pub(crate) fn push_denial(&mut self, capability: String, target: String, reason: DenialReason) {
        let target = clip(&target, 1024);
        let at_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.denials.push(Denial {
            capability,
            target,
            reason,
            at_ms,
        });
    }

    fn push_log(&mut self, level: u8, message: String) {
        if self.logs.len() >= LOG_LIMIT {
            return;
        }
        self.logs.push((level, clip(&message, LOG_BYTES)));
    }
}

enum FsAuth {
    Allow(Resolved),
    Deny {
        reason: DenialReason,
        target: String,
    },
}

fn authorize_fs(ctx: &RunCtx, capability: &str, raw: &str, use_target: bool) -> Result<FsAuth> {
    ctx.guard_limit()?;
    if !ctx.grants.declares(capability) {
        return Ok(FsAuth::Deny {
            reason: DenialReason::UndeclaredCapability,
            target: raw.to_string(),
        });
    }
    if path::reject_syntax(raw) {
        return Ok(FsAuth::Deny {
            reason: DenialReason::OutOfScope,
            target: raw.to_string(),
        });
    }
    let resolved = match path::resolve(raw) {
        Some(resolved) => resolved,
        None => {
            return Ok(FsAuth::Deny {
                reason: DenialReason::OutOfScope,
                target: raw.to_string(),
            });
        }
    };
    if use_target && resolved.broken_symlink {
        return Ok(FsAuth::Deny {
            reason: DenialReason::OutOfScope,
            target: display_path(&resolved.own),
        });
    }
    let candidate = if use_target {
        &resolved.scope
    } else {
        &resolved.own
    };
    match grant::check(
        &ctx.grants,
        capability,
        CheckTarget::Path(candidate),
        &ctx.denylist,
    ) {
        Ok(()) => Ok(FsAuth::Allow(resolved)),
        Err(reason) => Ok(FsAuth::Deny {
            reason,
            target: display_path(candidate),
        }),
    }
}

fn authorize_bare(ctx: &mut RunCtx, capability: &str) -> Result<bool> {
    ctx.guard_limit()?;
    match grant::check(&ctx.grants, capability, CheckTarget::None, &ctx.denylist) {
        Ok(()) => Ok(true),
        Err(reason) => {
            ctx.push_denial(capability.to_string(), String::new(), reason);
            Ok(false)
        }
    }
}

fn wit_denial(
    capability: &str,
    target: &str,
    reason: DenialReason,
) -> crate::novbot::skill::types::Denial {
    crate::novbot::skill::types::Denial {
        capability: capability.to_string(),
        target: target.to_string(),
        reason: reason_str(reason).to_string(),
    }
}

fn reason_str(reason: DenialReason) -> &'static str {
    match reason {
        DenialReason::UndeclaredCapability => "undeclared_capability",
        DenialReason::OutOfScope => "out_of_scope",
        DenialReason::CapDisabled => "cap_disabled",
        DenialReason::PolicyDenied => "policy_denied",
    }
}

fn io_error(err: std::io::Error) -> crate::novbot::skill::types::HostError {
    let message = err.to_string();
    if err.kind() == ErrorKind::NotFound {
        crate::novbot::skill::types::HostError::NotFound(message)
    } else {
        crate::novbot::skill::types::HostError::Io(message)
    }
}

impl crate::novbot::skill::fs::Host for RunCtx {
    fn read(
        &mut self,
        path: String,
        max_bytes: u32,
    ) -> Result<Result<Vec<u8>, crate::novbot::skill::types::HostError>> {
        match authorize_fs(self, "fs.read", &path, true)? {
            FsAuth::Deny { reason, target } => {
                self.push_denial("fs.read".to_string(), target.clone(), reason);
                Ok(Err(crate::novbot::skill::types::HostError::Denied(
                    wit_denial("fs.read", &target, reason),
                )))
            }
            FsAuth::Allow(resolved) => {
                if resolved.final_symlink {
                    return Ok(Err(crate::novbot::skill::types::HostError::Io(
                        "refusing to follow symlink".into(),
                    )));
                }
                match read_limited(&resolved.own, max_bytes) {
                    Ok(bytes) => Ok(Ok(bytes)),
                    Err(err) => Ok(Err(io_error(err))),
                }
            }
        }
    }

    fn stat(
        &mut self,
        path: String,
    ) -> Result<Result<crate::novbot::skill::fs::FileStat, crate::novbot::skill::types::HostError>>
    {
        match authorize_fs(self, "fs.stat", &path, false)? {
            FsAuth::Deny { reason, target } => {
                self.push_denial("fs.stat".to_string(), target.clone(), reason);
                Ok(Err(crate::novbot::skill::types::HostError::Denied(
                    wit_denial("fs.stat", &target, reason),
                )))
            }
            FsAuth::Allow(resolved) => match stat_path(&resolved.own) {
                Ok(stat) => Ok(Ok(stat)),
                Err(err) => Ok(Err(io_error(err))),
            },
        }
    }

    fn list(
        &mut self,
        dir: String,
        max_depth: u8,
        max_entries: u32,
    ) -> Result<Result<Vec<String>, crate::novbot::skill::types::HostError>> {
        match authorize_fs(self, "fs.list", &dir, true)? {
            FsAuth::Deny { reason, target } => {
                self.push_denial("fs.list".to_string(), target.clone(), reason);
                Ok(Err(crate::novbot::skill::types::HostError::Denied(
                    wit_denial("fs.list", &target, reason),
                )))
            }
            FsAuth::Allow(resolved) => {
                if resolved.final_symlink {
                    return Ok(Err(crate::novbot::skill::types::HostError::Io(
                        "refusing to follow symlink".into(),
                    )));
                }
                match list_dir(&resolved.own, max_depth, max_entries) {
                    Ok(entries) => Ok(Ok(entries)),
                    Err(err) => Ok(Err(io_error(err))),
                }
            }
        }
    }
}

impl crate::novbot::skill::net::Host for RunCtx {
    fn listening_ports(
        &mut self,
    ) -> Result<
        Result<
            Vec<crate::novbot::skill::net::ListenSocket>,
            crate::novbot::skill::types::HostError,
        >,
    > {
        if !authorize_bare(self, "net.listening_ports.read")? {
            let denial = self.denials.last().expect("denial recorded");
            return Ok(Err(crate::novbot::skill::types::HostError::Denied(
                wit_denial(&denial.capability, &denial.target, denial.reason),
            )));
        }
        match read_listening_ports() {
            Ok(sockets) => Ok(Ok(sockets)),
            Err(err) => Ok(Err(crate::novbot::skill::types::HostError::Io(err))),
        }
    }
}

impl crate::novbot::skill::sys::Host for RunCtx {
    fn info(&mut self) -> Result<Result<String, crate::novbot::skill::types::HostError>> {
        if !authorize_bare(self, "sys.info.read")? {
            let denial = self.denials.last().expect("denial recorded");
            return Ok(Err(crate::novbot::skill::types::HostError::Denied(
                wit_denial(&denial.capability, &denial.target, denial.reason),
            )));
        }
        Ok(Ok(system_info()))
    }

    fn time_sync(&mut self) -> Result<Result<String, crate::novbot::skill::types::HostError>> {
        if !authorize_bare(self, "sys.time_sync.read")? {
            let denial = self.denials.last().expect("denial recorded");
            return Ok(Err(crate::novbot::skill::types::HostError::Denied(
                wit_denial(&denial.capability, &denial.target, denial.reason),
            )));
        }
        Ok(Ok(time_sync_json()))
    }

    fn env_get(
        &mut self,
        key: String,
    ) -> Result<Result<Option<String>, crate::novbot::skill::types::HostError>> {
        self.guard_limit()?;
        match grant::check(
            &self.grants,
            "env.read",
            CheckTarget::Env(&key),
            &self.denylist,
        ) {
            Ok(()) => Ok(Ok(std::env::var(&key).ok())),
            Err(reason) => {
                self.push_denial("env.read".to_string(), key.clone(), reason);
                Ok(Err(crate::novbot::skill::types::HostError::Denied(
                    wit_denial("env.read", &key, reason),
                )))
            }
        }
    }
}

impl crate::novbot::skill::types::Host for RunCtx {}

impl crate::novbot::skill::log::Host for RunCtx {
    fn log(&mut self, level: u8, message: String) -> Result<()> {
        self.guard_limit()?;
        self.push_log(level, message);
        Ok(())
    }
}

fn read_limited(path: &Path, max_bytes: u32) -> std::io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let limit = u64::from(max_bytes.min(MAX_READ_BYTES));
    let mut buf = Vec::new();
    file.take(limit).read_to_end(&mut buf)?;
    Ok(buf)
}

fn stat_path(path: &Path) -> std::io::Result<crate::novbot::skill::fs::FileStat> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(crate::novbot::skill::fs::FileStat {
            exists: true,
            kind: kind_name(&meta).to_string(),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            size: meta.size(),
            mtime_unix_ms: mtime_ms(&meta),
        }),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(crate::novbot::skill::fs::FileStat {
            exists: false,
            kind: "other".to_string(),
            mode: 0,
            uid: 0,
            gid: 0,
            size: 0,
            mtime_unix_ms: 0,
        }),
        Err(err) => Err(err),
    }
}

fn kind_name(meta: &fs::Metadata) -> &'static str {
    let kind = meta.file_type();
    if kind.is_symlink() {
        "symlink"
    } else if kind.is_dir() {
        "dir"
    } else if kind.is_file() {
        "file"
    } else {
        "other"
    }
}

fn mtime_ms(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn list_dir(dir: &Path, max_depth: u8, max_entries: u32) -> std::io::Result<Vec<String>> {
    if max_depth == 0 || max_entries == 0 {
        return Ok(Vec::new());
    }
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::other("refusing to follow symlink"));
    }
    if !meta.is_dir() {
        return Err(std::io::Error::other("not a directory"));
    }
    let mut out = Vec::new();
    let mut stack = vec![ListFrame {
        dir: dir.to_path_buf(),
        prefix: String::new(),
        depth: 1,
    }];
    while let Some(frame) = stack.pop() {
        if out.len() as u32 >= max_entries {
            break;
        }
        let mut children = Vec::new();
        for entry in fs::read_dir(&frame.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            let rel = if frame.prefix.is_empty() {
                name
            } else {
                format!("{}/{}", frame.prefix, name)
            };
            let file_type = entry.file_type()?;
            children.push((rel, entry.path(), file_type));
        }
        children.sort_by(|left, right| left.0.cmp(&right.0));
        let mut nested = Vec::new();
        for (rel, path, file_type) in children {
            if out.len() as u32 >= max_entries {
                break;
            }
            let descend = file_type.is_dir() && !file_type.is_symlink() && frame.depth < max_depth;
            out.push(rel.clone());
            if descend {
                nested.push(ListFrame {
                    dir: path,
                    prefix: rel,
                    depth: frame.depth + 1,
                });
            }
        }
        // Push in reverse so lexicographic order is visited first.
        for frame in nested.into_iter().rev() {
            stack.push(frame);
        }
    }
    Ok(out)
}

struct ListFrame {
    dir: PathBuf,
    prefix: String,
    depth: u8,
}

fn read_listening_ports() -> Result<Vec<crate::novbot::skill::net::ListenSocket>, String> {
    if !Path::new("/proc/net").is_dir() {
        return Err("proc net is not available".to_string());
    }
    let files = [
        ("/proc/net/tcp", "tcp"),
        ("/proc/net/tcp6", "tcp6"),
        ("/proc/net/udp", "udp"),
        ("/proc/net/udp6", "udp6"),
    ];
    let mut sockets = Vec::new();
    for (path, proto) in files {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(err.to_string()),
        };
        for line in text.lines().skip(1) {
            if let Some(socket) = parse_proc_socket(proto, line) {
                sockets.push(socket);
            }
        }
    }
    Ok(sockets)
}

fn parse_proc_socket(proto: &str, line: &str) -> Option<crate::novbot::skill::net::ListenSocket> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 8 {
        return None;
    }
    let (local_addr, local_port) = fields[1].split_once(':')?;
    let (_remote_addr, remote_port) = fields[2].split_once(':')?;
    let state = fields[3];
    let port = u16::from_str_radix(local_port, 16).ok()?;
    let remote = u16::from_str_radix(remote_port, 16).ok()?;
    let listening = if proto.starts_with("tcp") {
        state.eq_ignore_ascii_case("0A")
    } else {
        remote == 0
    };
    if !listening {
        return None;
    }
    let addr = if local_addr.len() == 32 {
        parse_ipv6(local_addr)?
    } else {
        parse_ipv4(local_addr)?
    };
    let uid = fields[7].parse().ok()?;
    Some(crate::novbot::skill::net::ListenSocket {
        proto: proto.to_string(),
        addr,
        port,
        uid,
    })
}

fn parse_ipv4(hex: &str) -> Option<String> {
    if hex.len() != 8 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(format!(
        "{}.{}.{}.{}",
        value & 0xff,
        (value >> 8) & 0xff,
        (value >> 16) & 0xff,
        (value >> 24) & 0xff
    ))
}

fn parse_ipv6(hex: &str) -> Option<String> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for index in 0..4 {
        let word = u32::from_str_radix(&hex[index * 8..index * 8 + 8], 16).ok()?;
        bytes[index * 4] = (word & 0xff) as u8;
        bytes[index * 4 + 1] = ((word >> 8) & 0xff) as u8;
        bytes[index * 4 + 2] = ((word >> 16) & 0xff) as u8;
        bytes[index * 4 + 3] = ((word >> 24) & 0xff) as u8;
    }
    Some(std::net::Ipv6Addr::from(bytes).to_string())
}

fn system_info() -> String {
    let (hostname, kernel) = uname_names();
    let release = os_release_fields();
    json!({
        "hostname": hostname,
        "os_release": {
            "name": release.get("NAME").cloned().unwrap_or_default(),
            "id": release.get("ID").cloned().unwrap_or_default(),
            "version": release.get("VERSION").cloned().unwrap_or_default(),
            "version_id": release.get("VERSION_ID").cloned().unwrap_or_default(),
            "pretty_name": release.get("PRETTY_NAME").cloned().unwrap_or_default(),
        },
        "kernel": kernel,
        "arch": std::env::consts::ARCH,
    })
    .to_string()
}

fn os_release_fields() -> std::collections::BTreeMap<String, String> {
    let mut fields = std::collections::BTreeMap::new();
    let Ok(text) = fs::read_to_string("/etc/os-release") else {
        return fields;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        fields.insert(key.to_string(), value.to_string());
    }
    fields
}

fn uname_names() -> (String, String) {
    let mut buffer = unsafe { std::mem::zeroed::<libc::utsname>() };
    // `uname` fills a caller-owned buffer. Strings are read up to the first NUL.
    let rc = unsafe { libc::uname(&mut buffer) };
    if rc != 0 {
        return ("unknown".to_string(), "unknown".to_string());
    }
    (c_string(&buffer.nodename), c_string(&buffer.release))
}

fn c_string(bytes: &[libc::c_char]) -> String {
    let raw: Vec<u8> = bytes
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    String::from_utf8_lossy(&raw).into_owned()
}

fn time_sync_json() -> String {
    let probe = probe_time_sync();
    json!({
        "synced": probe.synced,
        "source": probe.source,
        "offset_ms": probe.offset_ms,
    })
    .to_string()
}

struct TimeProbe {
    synced: bool,
    source: Option<String>,
    offset_ms: Option<i64>,
}

fn probe_time_sync() -> TimeProbe {
    if Path::new("/run/systemd/timesync/synchronized").is_file() {
        return TimeProbe {
            synced: true,
            source: Some("systemd-timesyncd".to_string()),
            offset_ms: kernel_offset_ms(),
        };
    }
    if let Some(offset_ms) = kernel_offset_ms() {
        return TimeProbe {
            synced: true,
            source: Some("kernel".to_string()),
            offset_ms: Some(offset_ms),
        };
    }
    TimeProbe {
        synced: false,
        source: None,
        offset_ms: None,
    }
}

#[cfg(target_os = "linux")]
fn kernel_offset_ms() -> Option<i64> {
    let mut status = unsafe { std::mem::zeroed::<libc::timex>() };
    let rc = unsafe { libc::adjtimex(&mut status) };
    if rc < 0 {
        return None;
    }
    const TIME_ERROR: i32 = 5;
    const STA_UNSYNC: i32 = 0x0040;
    const STA_NANO: i32 = 0x2000;
    if rc == TIME_ERROR || (status.status & STA_UNSYNC) != 0 {
        return None;
    }
    let offset = status.offset as i64;
    if status.status & STA_NANO != 0 {
        Some(offset / 1_000_000)
    } else {
        Some(offset / 1_000)
    }
}

#[cfg(not(target_os = "linux"))]
fn kernel_offset_ms() -> Option<i64> {
    None
}

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = bytes.len() as u64;
    for (index, byte) in bytes.iter().enumerate().step_by(64) {
        hash = hash
            .wrapping_mul(0x1000_0000_01b3)
            .wrapping_add(u64::from(*byte))
            .wrapping_add(index as u64);
    }
    hash
}

fn clip(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}
