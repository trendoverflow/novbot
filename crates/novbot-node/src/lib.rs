// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Node library. The daemon binary lives in `main.rs`.

pub mod execute;
pub mod skills;

#[cfg(test)]
mod hub_run;

pub use execute::{apply_dispatch_overlay, execute_spec};
