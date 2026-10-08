// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Host errors returned by the skill imports.

use std::fmt;

/// Error from a host import. `Denied` is the WIT `denied(denial)` case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    Denied {
        capability: String,
        target: String,
        reason: String,
    },
    NotFound(String),
    Io(String),
    LimitExceeded(String),
    Invalid(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied {
                capability,
                target,
                reason,
            } => write!(
                f,
                "denied capability={capability} target={target} reason={reason}"
            ),
            Self::NotFound(message) => write!(f, "not_found: {message}"),
            Self::Io(message) => write!(f, "io: {message}"),
            Self::LimitExceeded(message) => write!(f, "limit_exceeded: {message}"),
            Self::Invalid(message) => write!(f, "invalid: {message}"),
        }
    }
}

impl std::error::Error for HostError {}
