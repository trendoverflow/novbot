// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Center-authored schedules: interval and/or 5-field cron (UTC). Survives node restart via pull-on-boot.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Schedule {
    pub spec_id: String,
    /// Interval schedule (seconds). Mutually optional with `cron` — at least one required to fire.
    #[serde(default, deserialize_with = "deser_opt_u64")]
    pub interval_secs: Option<u64>,
    /// 5-field cron: minute hour day-of-month month day-of-week (UTC). Supports *, N, */N, lists.
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Accept both `30` and `"30"` and null for optional interval.
fn deser_opt_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Option<u64>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("optional u64")
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            if v < 0 {
                return Err(E::custom("negative interval"));
            }
            Ok(Some(v as u64))
        }
        fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
            s.parse::<u64>().map(Some).map_err(E::custom)
        }
    }
    deserializer.deserialize_any(V)
}

impl Schedule {
    pub fn interval(&self) -> Option<Duration> {
        self.interval_secs.map(|s| Duration::from_secs(s.max(1)))
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
        if schedule_due(s, last_run, now_secs) {
            out.push(s.spec_id.clone());
        }
    }
    out
}

fn schedule_due(s: &Schedule, last_run: &HashMap<String, u64>, now_secs: u64) -> bool {
    let last = last_run.get(&s.spec_id).copied().unwrap_or(0);

    let mut due = false;

    if let Some(interval) = s.interval_secs {
        let interval = interval.max(1);
        if now_secs.saturating_sub(last) >= interval {
            due = true;
        }
    }

    if let Some(expr) = &s.cron {
        if cron_matches(expr, now_secs) {
            // Fire at most once per matching UTC minute.
            let minute_bucket = now_secs / 60;
            let last_bucket = last / 60;
            if minute_bucket > last_bucket || last == 0 {
                due = true;
            }
        }
    }

    // If neither interval nor cron configured, never due.
    if s.interval_secs.is_none() && s.cron.as_ref().map(|c| c.trim().is_empty()).unwrap_or(true) {
        return false;
    }

    due
}

/// Minimal 5-field cron matcher (UTC): min hour dom month dow.
pub fn cron_matches(expr: &str, unix_secs: u64) -> bool {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    let (minute, hour, day, month, weekday) = utc_parts(unix_secs);
    field_match(fields[0], minute, 0, 59)
        && field_match(fields[1], hour, 0, 23)
        && field_match(fields[2], day, 1, 31)
        && field_match(fields[3], month, 1, 12)
        && field_match(fields[4], weekday, 0, 6)
}

fn utc_parts(unix_secs: u64) -> (u32, u32, u32, u32, u32) {
    // Civil UTC from unix seconds (no chrono dependency).
    let days = (unix_secs / 86_400) as i64;
    let secs_of_day = (unix_secs % 86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;

    // 1970-01-01 was Thursday = 4; we use 0=Sunday..6=Saturday like cron.
    let weekday = ((days + 4).rem_euclid(7)) as u32;

    let (year, month, day) = civil_from_days(days + 719_468); // shift to civil algorithm epoch
    let _ = year;
    (minute, hour, day, month, weekday)
}

/// Howard Hinnant civil_from_days (days since 1970-01-01 + 719468 offset applied by caller).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64 + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn field_match(field: &str, value: u32, min: u32, max: u32) -> bool {
    if field == "*" {
        return true;
    }
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(step_str) = part.strip_prefix("*/") {
            if let Ok(step) = step_str.parse::<u32>() {
                if step > 0 && value >= min && ((value - min) % step == 0) {
                    return true;
                }
            }
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (a.parse::<u32>(), b.parse::<u32>()) {
                if value >= lo && value <= hi {
                    return true;
                }
            }
            continue;
        }
        if let Some((base, step_str)) = part.split_once('/') {
            if base == "*" {
                continue;
            }
            if let (Ok(start), Ok(step)) = (base.parse::<u32>(), step_str.parse::<u32>()) {
                if step > 0 && value >= start && (value - start) % step == 0 && value <= max {
                    return true;
                }
            }
            continue;
        }
        if let Ok(n) = part.parse::<u32>() {
            if n == value {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_when_never_run_interval() {
        let schedules = vec![Schedule {
            spec_id: "cpu".into(),
            interval_secs: Some(60),
            cron: None,
            enabled: true,
        }];
        let due = due_specs(
            &schedules,
            &HashMap::new(),
            UNIX_EPOCH + Duration::from_secs(100),
        );
        assert_eq!(due, vec!["cpu"]);
    }

    #[test]
    fn parses_cron_schedule() {
        let raw = r#"[{"spec_id":"cpu","cron":"*/5 * * * *","enabled":true}]"#;
        let s = parse_schedules_json(raw).unwrap();
        assert_eq!(s[0].cron.as_deref(), Some("*/5 * * * *"));
        assert!(s[0].interval_secs.is_none());
    }

    #[test]
    fn backward_compat_interval_number() {
        let raw = r#"[{"spec_id":"cpu","interval_secs":30,"enabled":true}]"#;
        let s = parse_schedules_json(raw).unwrap();
        assert_eq!(s[0].interval_secs, Some(30));
    }

    #[test]
    fn cron_every_minute() {
        assert!(cron_matches("* * * * *", 1_700_000_000));
    }

    #[test]
    fn cron_specific_minute() {
        // 1700000000 = 2023-11-14 22:13:20 UTC → minute 13
        let ts = 1_700_000_000u64;
        assert!(cron_matches("13 * * * *", ts));
        assert!(!cron_matches("14 * * * *", ts));
    }

    #[test]
    fn cron_step() {
        let ts = 1_700_000_000u64; // minute 13
        assert!(cron_matches("*/1 * * * *", ts));
        // 13 % 5 == 3, not 0 from min 0
        assert!(!cron_matches("*/5 * * * *", ts));
    }

    #[test]
    fn due_cron_once_per_minute() {
        let schedules = vec![Schedule {
            spec_id: "c".into(),
            interval_secs: None,
            cron: Some("* * * * *".into()),
            enabled: true,
        }];
        let now = UNIX_EPOCH + Duration::from_secs(100);
        let due1 = due_specs(&schedules, &HashMap::new(), now);
        assert_eq!(due1, vec!["c"]);
        let mut last = HashMap::new();
        last.insert("c".into(), 100);
        let due2 = due_specs(&schedules, &last, now);
        assert!(due2.is_empty());
        let due3 = due_specs(&schedules, &last, UNIX_EPOCH + Duration::from_secs(160));
        assert_eq!(due3, vec!["c"]);
    }
}
