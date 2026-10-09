// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Wasmtime component runtime for one skill run.
//!
//! This crate loads a `novbot:skill@1.0.0` component and serves the read-only
//! host catalog. It does not talk to center, dispatch, or the node main loop.

pub mod grant;
pub mod path;

mod deny;
mod host;
mod observe;
mod wasi_deny;

pub use grant::{CapabilityDef, DenialReason, ScopeRequirement, CAPABILITIES};
pub use host::{run, set_missing_os_release_fixture, RunRequest, SkillRuntime};
pub use observe::{set_observe_fixture, ObserveFixture};

use serde::Serialize;

/// Per-call host read clamp. This is not an SH-4 run quota.
pub const MAX_READ_BYTES: u32 = 256 * 1024;

/// Denials recorded in one run before the next host call traps the guest.
pub const DENIAL_LIMIT: usize = 16;

/// JSON result of one component run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunOutput {
    pub status: &'static str,
    pub error: Option<RunError>,
    pub denials: Vec<Denial>,
    pub partial: Option<String>,
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Denial {
    pub capability: String,
    pub target: String,
    pub reason: DenialReason,
    pub at_ms: u64,
}

impl RunOutput {
    pub(crate) fn ok(output: String) -> Self {
        Self {
            status: "ok",
            error: None,
            denials: Vec::new(),
            partial: None,
            output: Some(output),
        }
    }

    pub(crate) fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        denials: Vec<Denial>,
        partial: Option<String>,
    ) -> Self {
        Self {
            status: "error",
            error: Some(RunError {
                code: code.into(),
                message: message.into(),
            }),
            denials,
            partial,
            output: None,
        }
    }
}

wasmtime::component::bindgen!({
    world: "skill",
    path: "wit",
    imports: { default: trappable },
});
