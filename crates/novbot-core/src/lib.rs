// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Shared execution core: JSON specs, probes, schedules, and report retry spool.

pub mod probe;
pub mod retry;
pub mod schedule;
pub mod spec;

pub use probe::{run_probe, ProbeError, ProbeOutcome};
pub use retry::{write_egress_result, PendingReport, RetrySpool};
pub use schedule::{due_specs, parse_schedules_json, Schedule};
pub use spec::{parse_specs_json, Spec, SpecKind};
