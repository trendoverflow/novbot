// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Schedule {
    pub spec_id: String,
    pub interval_secs: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Schedule {
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs.max(1))
    }
}

pub fn parse_schedules_json(raw: &str) -> Result<Vec<Schedule>, serde_json::Error> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(raw)
}

pub fn due_specs(
    schedules: &[Schedule],
    last_run: &HashMap<String, u64>,
    now: SystemTime,
) -> Vec<String> {
    let now_secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut out = Vec::new();
    for s in schedules {
        if !s.enabled {
            continue;
        }
        let last = last_run.get(&s.spec_id).copied().unwrap_or(0);
        if now_secs.saturating_sub(last) >= s.interval_secs.max(1) {
            out.push(s.spec_id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_when_never_run() {
        let schedules = vec![Schedule {
            spec_id: "cpu".into(),
            interval_secs: 60,
            enabled: true,
        }];
        let due = due_specs(
            &schedules,
            &HashMap::new(),
            UNIX_EPOCH + Duration::from_secs(100),
        );
        assert_eq!(due, vec!["cpu"]);
    }
}
