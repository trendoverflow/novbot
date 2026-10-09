// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! SH-9 collectors. Fixed `/proc` and sysfs reads, plus `statvfs` and
//! `getifaddrs`. No shell, `ps`, `ifconfig`, `ip`, or `netstat`.
//!
//! `sys.metrics.read` JSON:
//! - `cpu.usage_percent`: non-idle share of the aggregate `/proc/stat` `cpu`
//!   line since boot. Idle includes `idle` and `iowait`.
//! - `cpu.load_avg_1m`, `cpu.load_avg_5m`, `cpu.load_avg_15m`: `/proc/loadavg`,
//!   or null when that file is absent.
//! - `memory.total_bytes`, `used_bytes`, `available_bytes`, `swap_total_bytes`,
//!   `swap_used_bytes`. `used_bytes` is `MemTotal - MemAvailable` when
//!   `MemAvailable` is present, otherwise `MemTotal - MemFree`. Values are
//!   kibibytes from `/proc/meminfo` converted to bytes.
//! - `disks[]`: `mount`, `fstype`, `total_bytes`, `used_bytes`,
//!   `available_bytes`, `inodes_total`, `inodes_used`, `inodes_available`.
//!   Mounts come from `/proc/mounts`. Byte and inode totals come from
//!   `statvfs`. Virtual fstypes are skipped. `/` is always kept.
//! - `disk_io[]`: `name`, `reads_completed`, `sectors_read`,
//!   `writes_completed`, `sectors_written`, `io_in_progress` from
//!   `/proc/diskstats`. When `/sys/block` exists, only those whole-disk names
//!   are kept. `loop*` and `ram*` are omitted.
//!
//! `proc.list.read` JSON key `processes[]`: `pid`, `ppid`, `name` (comm),
//! `uid`, `user`, `state`, `cpu_ticks`, `cpu_percent`, `memory_bytes`,
//! `start_time_unix_ms`. `cpu_percent` is `(utime + stime) / clock ticks /
//! seconds since start * 100`. Readers open `stat` and `status` only. They
//! never open `cmdline`, `environ`, or `exe`.
//!
//! `net.interfaces.read` JSON key `interfaces[]`: `name`, `addresses`,
//! `operstate`, `up`, `rx_bytes`, `rx_packets`, `rx_errors`, `tx_bytes`,
//! `tx_packets`, `tx_errors`. Counters come from `/proc/net/dev`. Link state
//! comes from `/sys/class/net/<name>/{operstate,flags}`. Addresses come from
//! `getifaddrs`. There is no connection table.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SKIP_FSTYPE: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "cgroup",
    "cgroup2",
    "pstore",
    "bpf",
    "tracefs",
    "debugfs",
    "securityfs",
    "configfs",
    "fusectl",
    "mqueue",
    "autofs",
    "rpc_pipefs",
    "binfmt_misc",
    "nsfs",
    "ramfs",
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
];

struct CpuTimes {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

struct Memory {
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
    swap_total_bytes: u64,
    swap_used_bytes: u64,
}

struct DiskIo {
    name: String,
    reads_completed: u64,
    sectors_read: u64,
    writes_completed: u64,
    sectors_written: u64,
    io_in_progress: u64,
}

struct MountLine {
    mount: String,
    fstype: String,
}

struct IfaceCounters {
    name: String,
    rx_bytes: u64,
    rx_packets: u64,
    rx_errors: u64,
    tx_bytes: u64,
    tx_packets: u64,
    tx_errors: u64,
}

pub(crate) fn metrics_json() -> Result<String, String> {
    let stat_path = Path::new("/proc/stat");
    let mem_path = Path::new("/proc/meminfo");
    if !stat_path.is_file() || !mem_path.is_file() {
        return Err("proc metrics are not available".into());
    }
    let stat_text = read_to_string(stat_path)?;
    let mem_text = read_to_string(mem_path)?;
    let load_text = fs::read_to_string("/proc/loadavg").ok();
    let diskstats = fs::read_to_string("/proc/diskstats").unwrap_or_default();
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let only = sys_block_names(Path::new("/sys/block"));
    let value = metrics_value(
        &stat_text,
        load_text.as_deref(),
        &mem_text,
        &diskstats,
        only.as_ref(),
        &mounts,
    )?;
    Ok(value.to_string())
}

pub(crate) fn processes_json() -> Result<String, String> {
    let root = Path::new("/proc");
    if !root.join("stat").is_file() {
        return Err("proc is not available".into());
    }
    let value = processes_from_root(root, now_unix(), clock_ticks())?;
    Ok(value.to_string())
}

pub(crate) fn interfaces_json() -> Result<String, String> {
    let dev = Path::new("/proc/net/dev");
    if !dev.is_file() {
        return Err("proc net dev is not available".into());
    }
    let text = read_to_string(dev)?;
    let addresses = interface_addresses();
    let value = interfaces_value(&text, Some(Path::new("/sys/class/net")), &addresses);
    Ok(value.to_string())
}

fn metrics_value(
    stat_text: &str,
    load_text: Option<&str>,
    mem_text: &str,
    diskstats: &str,
    only_disks: Option<&BTreeSet<String>>,
    mounts: &str,
) -> Result<Value, String> {
    let cpu = parse_cpu_times(stat_text).ok_or_else(|| "proc stat has no cpu line".to_string())?;
    let memory = parse_meminfo(mem_text).ok_or_else(|| "proc meminfo is incomplete".to_string())?;
    let load = load_text.and_then(parse_loadavg);
    let usage = cpu_usage_percent(&cpu);
    let disk_io = parse_diskstats(diskstats, only_disks);
    let disks = mount_usages(mounts);
    Ok(json!({
        "cpu": {
            "usage_percent": usage,
            "load_avg_1m": load.map(|row| row.0),
            "load_avg_5m": load.map(|row| row.1),
            "load_avg_15m": load.map(|row| row.2),
        },
        "memory": {
            "total_bytes": memory.total_bytes,
            "used_bytes": memory.used_bytes,
            "available_bytes": memory.available_bytes,
            "swap_total_bytes": memory.swap_total_bytes,
            "swap_used_bytes": memory.swap_used_bytes,
        },
        "disks": disks,
        "disk_io": disk_io.into_iter().map(|row| json!({
            "name": row.name,
            "reads_completed": row.reads_completed,
            "sectors_read": row.sectors_read,
            "writes_completed": row.writes_completed,
            "sectors_written": row.sectors_written,
            "io_in_progress": row.io_in_progress,
        })).collect::<Vec<_>>(),
    }))
}

fn processes_from_root(root: &Path, now_unix: u64, clk_tck: u64) -> Result<Value, String> {
    let stat_text = read_to_string(&root.join("stat"))?;
    let btime = parse_btime(&stat_text);
    let clk = clk_tck.max(1);
    let mut rows = Vec::new();
    let mut users: BTreeMap<u32, String> = BTreeMap::new();
    let entries = fs::read_dir(root).map_err(|err| err.to_string())?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let file_name = entry.file_name();
        let Some(pid_text) = file_name.to_str() else {
            continue;
        };
        if !pid_text.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Some(pid) = pid_text.parse::<u64>().ok() else {
            continue;
        };
        let dir = entry.path();
        // `stat` and `status` only. `cmdline`, `environ`, and `exe` stay closed.
        let Some(stat_body) = read_lossy(&dir.join("stat")) else {
            continue;
        };
        let Some(parsed) = parse_proc_stat(&stat_body) else {
            continue;
        };
        let status = read_lossy(&dir.join("status")).unwrap_or_default();
        let (status_uid, rss_kb) = parse_status(&status);
        let memory_bytes = rss_kb.map(|kb| kb.saturating_mul(1024)).unwrap_or(0);
        let cpu_ticks = parsed.utime.saturating_add(parsed.stime);
        let start_unix = btime.map(|boot| boot.saturating_add(parsed.starttime / clk));
        let cpu_percent = match start_unix {
            Some(start) => {
                let elapsed = now_unix.saturating_sub(start).max(1);
                (cpu_ticks as f64 / clk as f64) / elapsed as f64 * 100.0
            }
            None => 0.0,
        };
        let user = status_uid.map(|uid| users.entry(uid).or_insert_with(|| user_name(uid)).clone());
        rows.push(json!({
            "pid": pid,
            "ppid": parsed.ppid,
            "name": parsed.name,
            "uid": status_uid,
            "user": user,
            "state": parsed.state,
            "cpu_ticks": cpu_ticks,
            "cpu_percent": cpu_percent,
            "memory_bytes": memory_bytes,
            "start_time_unix_ms": start_unix.map(|secs| secs.saturating_mul(1000)),
        }));
    }
    rows.sort_by(|left, right| {
        left["pid"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&right["pid"].as_u64().unwrap_or(0))
    });
    Ok(json!({ "processes": rows }))
}

fn interfaces_value(
    dev_text: &str,
    sys_class_net: Option<&Path>,
    addresses: &BTreeMap<String, Vec<String>>,
) -> Value {
    let rows = parse_proc_net_dev(dev_text)
        .into_iter()
        .map(|row| {
            let (operstate, up) = link_state(sys_class_net, &row.name);
            let mut addrs = addresses.get(&row.name).cloned().unwrap_or_default();
            addrs.sort();
            addrs.dedup();
            json!({
                "name": row.name,
                "addresses": addrs,
                "operstate": operstate,
                "up": up,
                "rx_bytes": row.rx_bytes,
                "rx_packets": row.rx_packets,
                "rx_errors": row.rx_errors,
                "tx_bytes": row.tx_bytes,
                "tx_packets": row.tx_packets,
                "tx_errors": row.tx_errors,
            })
        })
        .collect::<Vec<_>>();
    json!({ "interfaces": rows })
}

fn parse_cpu_times(text: &str) -> Option<CpuTimes> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else {
            continue;
        };
        if label != "cpu" {
            continue;
        }
        let nums: Vec<u64> = parts.filter_map(|part| part.parse().ok()).collect();
        let n = |index: usize| nums.get(index).copied().unwrap_or(0);
        return Some(CpuTimes {
            user: n(0),
            nice: n(1),
            system: n(2),
            idle: n(3),
            iowait: n(4),
            irq: n(5),
            softirq: n(6),
            steal: n(7),
        });
    }
    None
}

fn cpu_usage_percent(times: &CpuTimes) -> f64 {
    let idle = times.idle.saturating_add(times.iowait);
    let total = times
        .user
        .saturating_add(times.nice)
        .saturating_add(times.system)
        .saturating_add(idle)
        .saturating_add(times.irq)
        .saturating_add(times.softirq)
        .saturating_add(times.steal);
    if total == 0 {
        return 0.0;
    }
    (total - idle) as f64 * 100.0 / total as f64
}

fn parse_loadavg(text: &str) -> Option<(f64, f64, f64)> {
    let mut parts = text.split_whitespace();
    let one = parts.next()?.parse().ok()?;
    let five = parts.next()?.parse().ok()?;
    let fifteen = parts.next()?.parse().ok()?;
    Some((one, five, fifteen))
}

fn parse_meminfo(text: &str) -> Option<Memory> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(kb) = rest
            .split_whitespace()
            .next()
            .and_then(|part| part.parse::<u64>().ok())
        else {
            continue;
        };
        map.insert(key.trim().to_string(), kb);
    }
    let total = *map.get("MemTotal")?;
    let available = map
        .get("MemAvailable")
        .copied()
        .or_else(|| map.get("MemFree").copied())?;
    let swap_total = map.get("SwapTotal").copied().unwrap_or(0);
    let swap_free = map.get("SwapFree").copied().unwrap_or(0);
    Some(Memory {
        total_bytes: total.saturating_mul(1024),
        used_bytes: total.saturating_sub(available).saturating_mul(1024),
        available_bytes: available.saturating_mul(1024),
        swap_total_bytes: swap_total.saturating_mul(1024),
        swap_used_bytes: swap_total.saturating_sub(swap_free).saturating_mul(1024),
    })
}

fn parse_btime(text: &str) -> Option<u64> {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("btime ") else {
            continue;
        };
        return rest.trim().parse().ok();
    }
    None
}

fn parse_diskstats(text: &str, only: Option<&BTreeSet<String>>) -> Vec<DiskIo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 12 {
            continue;
        }
        let name = fields[2];
        if name.starts_with("loop") || name.starts_with("ram") {
            continue;
        }
        if only.is_some_and(|names| !names.contains(name)) {
            continue;
        }
        let num = |index: usize| fields[index].parse::<u64>().unwrap_or(0);
        out.push(DiskIo {
            name: name.to_string(),
            reads_completed: num(3),
            sectors_read: num(5),
            writes_completed: num(7),
            sectors_written: num(9),
            io_in_progress: num(11),
        });
    }
    out
}

fn sys_block_names(sys_block: &Path) -> Option<BTreeSet<String>> {
    let entries = fs::read_dir(sys_block).ok()?;
    let mut names = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.is_empty() && !name.contains('.') {
            names.insert(name.to_string());
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

fn parse_mounts(text: &str) -> Vec<MountLine> {
    let mut out = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        out.push(MountLine {
            mount: unescape_mount(fields[1]),
            fstype: unescape_mount(fields[2]),
        });
    }
    out
}

fn mount_usages(text: &str) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for mount in parse_mounts(text) {
        if mount.mount != "/" && SKIP_FSTYPE.contains(&mount.fstype.as_str()) {
            continue;
        }
        if !seen.insert(mount.mount.clone()) {
            continue;
        }
        let Some(usage) = vfs_usage(&mount.mount) else {
            continue;
        };
        out.push(json!({
            "mount": mount.mount,
            "fstype": mount.fstype,
            "total_bytes": usage.total_bytes,
            "used_bytes": usage.used_bytes,
            "available_bytes": usage.available_bytes,
            "inodes_total": usage.inodes_total,
            "inodes_used": usage.inodes_used,
            "inodes_available": usage.inodes_available,
        }));
    }
    out
}

struct VfsUsage {
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
    inodes_total: u64,
    inodes_used: u64,
    inodes_available: u64,
}

fn vfs_usage(path: &str) -> Option<VfsUsage> {
    let mut bytes = path.as_bytes().to_vec();
    if bytes.contains(&0) {
        return None;
    }
    bytes.push(0);
    let mut buf = unsafe { std::mem::zeroed::<libc::statvfs>() };
    // `statvfs` fills a caller-owned buffer for one mount point.
    let rc = unsafe { libc::statvfs(bytes.as_ptr().cast(), &mut buf) };
    if rc != 0 {
        return None;
    }
    let frsize = buf.f_frsize as u64;
    let total = (buf.f_blocks as u64).saturating_mul(frsize);
    let free = (buf.f_bfree as u64).saturating_mul(frsize);
    let available = (buf.f_bavail as u64).saturating_mul(frsize);
    let inodes_total = buf.f_files as u64;
    let inodes_free = buf.f_ffree as u64;
    Some(VfsUsage {
        total_bytes: total,
        used_bytes: total.saturating_sub(free),
        available_bytes: available,
        inodes_total,
        inodes_used: inodes_total.saturating_sub(inodes_free),
        inodes_available: buf.f_favail as u64,
    })
}

fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &field[index + 1..index + 4];
            if let Ok(value) = u8::from_str_radix(octal, 8) {
                out.push(char::from(value));
                index += 4;
                continue;
            }
        }
        out.push(char::from(bytes[index]));
        index += 1;
    }
    out
}

struct ProcStat {
    name: String,
    state: String,
    ppid: u64,
    utime: u64,
    stime: u64,
    starttime: u64,
}

fn parse_proc_stat(text: &str) -> Option<ProcStat> {
    let text = text.trim();
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close <= open {
        return None;
    }
    let name = text[open + 1..close].to_string();
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    if rest.len() <= 19 {
        return None;
    }
    Some(ProcStat {
        name,
        state: rest[0].to_string(),
        ppid: rest[1].parse().ok()?,
        utime: rest[11].parse().ok()?,
        stime: rest[12].parse().ok()?,
        starttime: rest[19].parse().ok()?,
    })
}

fn parse_status(text: &str) -> (Option<u32>, Option<u64>) {
    let mut uid = None;
    let mut rss_kb = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            uid = rest
                .split_whitespace()
                .next()
                .and_then(|part| part.parse().ok());
        } else if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss_kb = rest
                .split_whitespace()
                .next()
                .and_then(|part| part.parse().ok());
        }
    }
    (uid, rss_kb)
}

fn parse_proc_net_dev(text: &str) -> Vec<IfaceCounters> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 11 {
            continue;
        }
        let num = |index: usize| fields[index].parse::<u64>().unwrap_or(0);
        out.push(IfaceCounters {
            name: name.to_string(),
            rx_bytes: num(0),
            rx_packets: num(1),
            rx_errors: num(2),
            tx_bytes: num(8),
            tx_packets: num(9),
            tx_errors: num(10),
        });
    }
    out
}

fn link_state(sys_class_net: Option<&Path>, name: &str) -> (String, bool) {
    let Some(root) = sys_class_net else {
        return ("unknown".to_string(), false);
    };
    let dir = root.join(name);
    let operstate = fs::read_to_string(dir.join("operstate"))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let flags_up = fs::read_to_string(dir.join("flags"))
        .ok()
        .is_some_and(|text| {
            let hex = text
                .trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            u32::from_str_radix(hex, 16).is_ok_and(|bits| bits & 1 == 1)
        });
    let up = operstate == "up" || flags_up;
    (operstate, up)
}

fn interface_addresses() -> BTreeMap<String, Vec<String>> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // `getifaddrs` allocates a list the caller frees with `freeifaddrs`.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return BTreeMap::new();
    }
    let _free = FreeIfAddrs(head);
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut cursor = head;
    while !cursor.is_null() {
        let ifa = unsafe { &*cursor };
        if !ifa.ifa_addr.is_null() {
            let family = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
            if family == libc::AF_INET || family == libc::AF_INET6 {
                if let (Some(name), Some(addr)) = (cstr(ifa.ifa_name), sockaddr_ip(ifa.ifa_addr)) {
                    map.entry(name).or_default().push(addr);
                }
            }
        }
        cursor = ifa.ifa_next;
    }
    map
}

struct FreeIfAddrs(*mut libc::ifaddrs);

impl Drop for FreeIfAddrs {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
}

fn sockaddr_ip(sa: *const libc::sockaddr) -> Option<String> {
    if sa.is_null() {
        return None;
    }
    let family = i32::from(unsafe { (*sa).sa_family });
    if family == libc::AF_INET {
        let v4 = unsafe { &*(sa.cast::<libc::sockaddr_in>()) };
        let ip = std::net::Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr));
        return Some(ip.to_string());
    }
    if family == libc::AF_INET6 {
        let v6 = unsafe { &*(sa.cast::<libc::sockaddr_in6>()) };
        let ip = std::net::Ipv6Addr::from(v6.sin6_addr.s6_addr);
        return Some(ip.to_string());
    }
    None
}

fn user_name(uid: u32) -> String {
    let mut cap = 16 * 1024;
    for _ in 0..4 {
        let mut buf = vec![0u8; cap];
        let mut pwd = unsafe { std::mem::zeroed::<libc::passwd>() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // `getpwuid_r` writes the password entry into `buf` and `pwd`.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE {
            cap = cap.saturating_mul(2);
            continue;
        }
        if rc != 0 || result.is_null() {
            return uid.to_string();
        }
        return cstr(unsafe { (*result).pw_name }).unwrap_or_else(|| uid.to_string());
    }
    uid.to_string()
}

fn cstr(ptr: *const libc::c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(ptr) };
    Some(text.to_string_lossy().into_owned())
}

fn clock_ticks() -> u64 {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(hz).unwrap_or(100).max(1)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn read_to_string(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|err| err.to_string())
}

fn read_lossy(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "s3cr3t-proc-cmdline-do-not-leak";

    fn assert_no_cmdline_keys(value: &Value) {
        const BANNED: &[&str] = &[
            "cmdline",
            "command_line",
            "command",
            "args",
            "argv",
            "exe",
            "environ",
            "environment",
        ];
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    assert!(!BANNED.contains(&key.as_str()), "banned field {key}");
                    assert_no_cmdline_keys(child);
                }
            }
            Value::Array(items) => {
                for item in items {
                    assert_no_cmdline_keys(item);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn metrics_fixture_covers_cpu_memory_and_disk_io() {
        let stat = "\
cpu  100 0 50 1000 10 0 5 5 0 0
cpu0 100 0 50 1000 10 0 5 5 0 0
btime 1700000000
";
        let mem = "\
MemTotal:       2048000 kB
MemFree:         512000 kB
MemAvailable:   1024000 kB
SwapTotal:       102400 kB
SwapFree:         51200 kB
";
        let diskstats = "\
   8       0 sda 10 0 100 1 4 0 40 1 0 5 6
   8       1 sda1 3 0 30 0 1 0 8 0 1 1 1
   7       0 loop0 9 0 9 0 0 0 0 0 0 0 0
";
        let mut only = BTreeSet::new();
        only.insert("sda".to_string());
        let value = metrics_value(
            stat,
            Some("0.50 0.25 0.10 1/100 9\n"),
            mem,
            diskstats,
            Some(&only),
            "",
        )
        .expect("metrics");
        let expected = 160.0 / 1170.0 * 100.0;
        let usage = value["cpu"]["usage_percent"].as_f64().unwrap();
        assert!((usage - expected).abs() < 1e-9, "{usage}");
        assert_eq!(value["cpu"]["load_avg_1m"], 0.5);
        assert_eq!(value["cpu"]["load_avg_5m"], 0.25);
        assert_eq!(value["cpu"]["load_avg_15m"], 0.1);
        assert_eq!(value["memory"]["total_bytes"], 2048000 * 1024);
        assert_eq!(value["memory"]["available_bytes"], 1024000 * 1024);
        assert_eq!(value["memory"]["used_bytes"], 1024000 * 1024);
        assert_eq!(value["memory"]["swap_total_bytes"], 102400 * 1024);
        assert_eq!(value["memory"]["swap_used_bytes"], 51200 * 1024);
        assert_eq!(value["disk_io"].as_array().unwrap().len(), 1);
        assert_eq!(value["disk_io"][0]["name"], "sda");
        assert_eq!(value["disk_io"][0]["reads_completed"], 10);
        assert_eq!(value["disk_io"][0]["sectors_read"], 100);
        assert_eq!(value["disk_io"][0]["writes_completed"], 4);
        assert_eq!(value["disk_io"][0]["sectors_written"], 40);
        assert_eq!(value["disk_io"][0]["io_in_progress"], 0);
        assert!(value["disks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn statvfs_reports_mount_usage_including_root_shape() {
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().to_str().unwrap().replace(' ', "\\040");
        let text = format!("/dev/disk {mount} tmpfs rw 0 0\nproc /proc proc rw 0 0\n");
        let disks = mount_usages(&text);
        assert_eq!(disks.len(), 1, "{disks:?}");
        assert_eq!(disks[0]["mount"], dir.path().to_str().unwrap());
        assert_eq!(disks[0]["fstype"], "tmpfs");
        assert!(disks[0]["total_bytes"].as_u64().unwrap() > 0);
        assert!(disks[0].get("inodes_total").is_some());
    }

    #[test]
    fn process_fixture_omits_cmdline_and_password() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stat"),
            "cpu  1 0 1 10 0 0 0 0 0 0\nbtime 1700000000\n",
        )
        .unwrap();
        let pid = root.path().join("42");
        fs::create_dir(&pid).unwrap();
        fs::write(
            pid.join("stat"),
            "42 (nginx) S 1 42 42 0 -1 4194560 100 0 0 0 50 25 0 0 20 0 1 0 100\n",
        )
        .unwrap();
        fs::write(
            pid.join("status"),
            "Name:\tnginx\nState:\tS (sleeping)\nPid:\t42\nPPid:\t1\nUid:\t0\t0\t0\t0\nVmRSS:\t    2048 kB\n",
        )
        .unwrap();
        let secret = format!("mysql --password={PASSWORD} --token={PASSWORD}");
        fs::write(pid.join("cmdline"), secret.as_bytes()).unwrap();
        fs::write(pid.join("environ"), format!("SECRET={PASSWORD}")).unwrap();
        fs::write(pid.join("exe"), secret.as_bytes()).unwrap();
        fs::create_dir(root.path().join("not-a-pid")).unwrap();
        fs::write(
            root.path().join("not-a-pid").join("cmdline"),
            secret.as_bytes(),
        )
        .unwrap();

        let value = processes_from_root(root.path(), 1_700_000_100, 100).unwrap();
        let text = value.to_string();
        assert!(!text.contains(PASSWORD), "{text}");
        assert_no_cmdline_keys(&value);
        let row = &value["processes"][0];
        assert_eq!(row["pid"], 42);
        assert_eq!(row["ppid"], 1);
        assert_eq!(row["name"], "nginx");
        assert_eq!(row["uid"], 0);
        assert_eq!(row["state"], "S");
        assert_eq!(row["cpu_ticks"], 75);
        assert_eq!(row["memory_bytes"], 2048 * 1024);
        assert_eq!(row["start_time_unix_ms"], 1_700_000_001_000u64);
        let cpu = row["cpu_percent"].as_f64().unwrap();
        let expected = 0.75 / 99.0 * 100.0;
        assert!((cpu - expected).abs() < 1e-9, "{cpu}");
        assert!(!row["user"].as_str().unwrap().is_empty());
    }

    #[test]
    fn interface_fixture_has_counters_and_no_connection_table() {
        let sys = tempfile::tempdir().unwrap();
        let eth = sys.path().join("eth0");
        let lo = sys.path().join("lo");
        fs::create_dir(&eth).unwrap();
        fs::create_dir(&lo).unwrap();
        fs::write(eth.join("operstate"), "up\n").unwrap();
        fs::write(eth.join("flags"), "0x1003\n").unwrap();
        fs::write(lo.join("operstate"), "unknown\n").unwrap();
        fs::write(lo.join("flags"), "0x9\n").unwrap();
        let dev = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
  eth0: 1000 10 1 0 0 0 0 0 2000 20 2 0 0 0 0 0
    lo: 5 1 0 0 0 0 0 0 6 2 0 0 0 0 0 0
";
        let mut addresses = BTreeMap::new();
        addresses.insert("eth0".to_string(), vec!["192.0.2.10".to_string()]);
        let value = interfaces_value(dev, Some(sys.path()), &addresses);
        assert!(value.get("connections").is_none());
        let rows = value["interfaces"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "eth0");
        assert_eq!(rows[0]["addresses"][0], "192.0.2.10");
        assert_eq!(rows[0]["operstate"], "up");
        assert_eq!(rows[0]["up"], true);
        assert_eq!(rows[0]["rx_bytes"], 1000);
        assert_eq!(rows[0]["rx_packets"], 10);
        assert_eq!(rows[0]["rx_errors"], 1);
        assert_eq!(rows[0]["tx_bytes"], 2000);
        assert_eq!(rows[0]["tx_packets"], 20);
        assert_eq!(rows[0]["tx_errors"], 2);
        assert_eq!(rows[1]["name"], "lo");
        assert_eq!(rows[1]["up"], true);
        assert_eq!(rows[1]["operstate"], "unknown");
        let text = value.to_string();
        assert!(!text.contains("TIME_WAIT"));
        assert!(text.contains("rx_bytes"));
    }

    #[test]
    fn live_collectors_error_when_proc_is_absent() {
        if !Path::new("/proc/stat").is_file() {
            let err = metrics_json().unwrap_err();
            assert!(err.contains("proc"), "{err}");
            let err = processes_json().unwrap_err();
            assert!(err.contains("proc"), "{err}");
        }
        if !Path::new("/proc/net/dev").is_file() {
            let err = interfaces_json().unwrap_err();
            assert!(err.contains("proc"), "{err}");
        }
    }

    #[test]
    fn comm_with_parentheses_parses() {
        let stat = "7 (my ) proc) R 1 7 7 0 -1 0 0 0 0 0 1 2 0 0 20 0 1 0 50\n";
        let parsed = parse_proc_stat(stat).unwrap();
        assert_eq!(parsed.name, "my ) proc");
        assert_eq!(parsed.state, "R");
        assert_eq!(parsed.ppid, 1);
        assert_eq!(parsed.utime, 1);
        assert_eq!(parsed.stime, 2);
        assert_eq!(parsed.starttime, 50);
    }
}
