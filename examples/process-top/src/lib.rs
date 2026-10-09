// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `process-top` 1.0.0. Returns the top processes from `proc.processes`.
//!
//! The payload copies name, pid, ppid, user, state, CPU, memory, and start
//! time. It does not copy command lines, argv, or environment.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

const TOP: usize = 10;

fn check(_params: serde_json::Value) -> Result<serde_json::Value, String> {
    let raw = novbot_skill_sdk::proc::processes().map_err(|err| err.to_string())?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|err| format!("proc.processes: {err}"))?;
    let processes = parsed
        .get("processes")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(serde_json::json!({
        "by_cpu": top_by(&processes, "cpu_percent"),
        "by_memory": top_by(&processes, "memory_bytes"),
    }))
}

fn top_by(processes: &[serde_json::Value], key: &str) -> Vec<serde_json::Value> {
    let mut rows = processes.to_vec();
    rows.sort_by(|left, right| {
        let left_key = json_f64(left, key);
        let right_key = json_f64(right, key);
        right_key
            .partial_cmp(&left_key)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| json_u64(left, "pid").cmp(&json_u64(right, "pid")))
    });
    rows.truncate(TOP);
    rows.into_iter().map(|row| public_process(&row)).collect()
}

fn public_process(row: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "pid": row["pid"],
        "ppid": row["ppid"],
        "name": row["name"],
        "uid": row["uid"],
        "user": row["user"],
        "state": row["state"],
        "cpu_ticks": row["cpu_ticks"],
        "cpu_percent": row["cpu_percent"],
        "memory_bytes": row["memory_bytes"],
        "start_time_unix_ms": row["start_time_unix_ms"],
    })
}

fn json_f64(value: &serde_json::Value, key: &str) -> f64 {
    value.get(key).and_then(|item| item.as_f64()).unwrap_or(0.0)
}

fn json_u64(value: &serde_json::Value, key: &str) -> u64 {
    value.get(key).and_then(|item| item.as_u64()).unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::check;
    use novbot_skill_sdk::testing::{with_host, MockHost};
    use serde_json::Value;

    const PASSWORD: &str = "s3cr3t-proc-cmdline-do-not-leak";

    fn assert_no_cmdline(value: &Value) {
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
                    assert!(!BANNED.contains(&key.as_str()), "{key}");
                    assert_no_cmdline(child);
                }
            }
            Value::Array(items) => {
                for item in items {
                    assert_no_cmdline(item);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn ranks_processes_without_command_lines() {
        let mut host = MockHost::new();
        host.processes = serde_json::json!({
            "processes": [
                {
                    "pid": 2,
                    "ppid": 1,
                    "name": "busy",
                    "uid": 0,
                    "user": "root",
                    "state": "R",
                    "cpu_ticks": 10,
                    "cpu_percent": 9.0,
                    "memory_bytes": 100,
                    "start_time_unix_ms": 1,
                    "cmdline": format!("--password={PASSWORD}"),
                    "command": PASSWORD,
                    "argv": [PASSWORD],
                    "exe": format!("/usr/bin/busy --password={PASSWORD}")
                },
                {
                    "pid": 3,
                    "ppid": 1,
                    "name": "fat",
                    "uid": 1,
                    "user": "bin",
                    "state": "S",
                    "cpu_ticks": 1,
                    "cpu_percent": 0.1,
                    "memory_bytes": 5000,
                    "start_time_unix_ms": 2
                }
            ]
        })
        .to_string();
        let (value, host) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert_eq!(value["by_cpu"][0]["name"], "busy");
        assert_eq!(value["by_cpu"][0]["pid"], 2);
        assert_eq!(value["by_cpu"][0]["user"], "root");
        assert_eq!(value["by_memory"][0]["name"], "fat");
        assert_eq!(value["by_memory"][0]["memory_bytes"], 5000);
        let text = value.to_string();
        assert!(!text.contains(PASSWORD), "{text}");
        assert_no_cmdline(&value);
        assert!(host.calls.iter().any(|call| call == "proc.processes"));
    }
}
