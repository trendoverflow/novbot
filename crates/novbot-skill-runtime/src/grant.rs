// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Grant grammar and the capability table.
//!
//! Host checks walk [`CAPABILITIES`]. A later host function is a new row plus
//! a host function; denial order stays in [`check`].

use crate::deny::Denylist;
use crate::path;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Why a host call was refused. `cap_disabled` is reserved and not produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialReason {
    UndeclaredCapability,
    OutOfScope,
    CapDisabled,
    PolicyDenied,
}

/// How a catalog capability is scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRequirement {
    /// The grant forbids a scope. The name ends in `.read`.
    None,
    /// `fs.read`, `fs.stat`, and `fs.list` require a path glob.
    PathGlob,
    /// `env.read` requires an environment-key scope.
    EnvKey,
}

#[derive(Debug, Clone, Copy)]
pub struct CapabilityDef {
    pub name: &'static str,
    pub scope: ScopeRequirement,
}

/// SH-3 host functions. SH-9 names are intentionally absent.
pub static CAPABILITIES: &[CapabilityDef] = &[
    CapabilityDef {
        name: "fs.read",
        scope: ScopeRequirement::PathGlob,
    },
    CapabilityDef {
        name: "fs.stat",
        scope: ScopeRequirement::PathGlob,
    },
    CapabilityDef {
        name: "fs.list",
        scope: ScopeRequirement::PathGlob,
    },
    CapabilityDef {
        name: "net.listening_ports.read",
        scope: ScopeRequirement::None,
    },
    CapabilityDef {
        name: "sys.info.read",
        scope: ScopeRequirement::None,
    },
    CapabilityDef {
        name: "sys.time_sync.read",
        scope: ScopeRequirement::None,
    },
    CapabilityDef {
        name: "env.read",
        scope: ScopeRequirement::EnvKey,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    /// The string is not a grant, or the scope does not match the capability.
    Malformed(String),
    /// The name is not in the SH-3 table (including later-phase capabilities).
    Unsupported(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(grant) => write!(f, "malformed grant: {grant}"),
            Self::Unsupported(grant) => write!(f, "capability not supported: {grant}"),
        }
    }
}

#[derive(Debug, Clone)]
enum Scope {
    Path(String),
    Env(EnvPattern),
    Bare,
}

#[derive(Debug, Clone)]
struct EnvPattern {
    prefix: String,
    wildcard: bool,
}

impl EnvPattern {
    fn matches(&self, key: &str) -> bool {
        if self.wildcard {
            key.starts_with(&self.prefix)
        } else {
            key == self.prefix
        }
    }
}

#[derive(Debug, Clone)]
pub struct GrantSet {
    by_name: BTreeMap<String, Vec<Scope>>,
}

impl GrantSet {
    pub fn parse(grants: &[&str]) -> Result<Self, GrantError> {
        let mut by_name: BTreeMap<String, Vec<Scope>> = BTreeMap::new();
        for grant in grants {
            let (name, scope) = split_grant(grant)?;
            let def = CAPABILITIES.iter().find(|def| def.name == name);
            let Some(def) = def else {
                return Err(GrantError::Unsupported((*grant).to_string()));
            };
            let parsed = match (def.scope, scope) {
                (ScopeRequirement::None, None) => Scope::Bare,
                (ScopeRequirement::None, Some(_)) => {
                    return Err(GrantError::Malformed((*grant).to_string()));
                }
                (ScopeRequirement::PathGlob, Some(scope)) if path::valid_path_glob(scope) => {
                    Scope::Path(scope.to_string())
                }
                (ScopeRequirement::PathGlob, _) => {
                    return Err(GrantError::Malformed((*grant).to_string()));
                }
                (ScopeRequirement::EnvKey, Some(scope)) => {
                    let Some(pattern) = parse_env_scope(scope) else {
                        return Err(GrantError::Malformed((*grant).to_string()));
                    };
                    Scope::Env(pattern)
                }
                (ScopeRequirement::EnvKey, None) => {
                    return Err(GrantError::Malformed((*grant).to_string()));
                }
            };
            by_name.entry(name.to_string()).or_default().push(parsed);
        }
        Ok(Self { by_name })
    }

    pub fn declares(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    fn path_matches(&self, name: &str, candidate: &Path) -> bool {
        self.by_name.get(name).is_some_and(|scopes| {
            scopes.iter().any(|scope| match scope {
                Scope::Path(glob) => path::glob_matches(glob, candidate),
                _ => false,
            })
        })
    }

    fn env_matches(&self, name: &str, key: &str) -> bool {
        self.by_name.get(name).is_some_and(|scopes| {
            scopes.iter().any(|scope| match scope {
                Scope::Env(pattern) => pattern.matches(key),
                _ => false,
            })
        })
    }
}

pub enum CheckTarget<'a> {
    None,
    Path(&'a Path),
    Env(&'a str),
}

/// Denial order: undeclared, out of scope, cap_disabled, policy.
///
/// `cap_disabled` is reserved. SH-3 has no switch that produces it.
pub fn check(
    grants: &GrantSet,
    name: &str,
    target: CheckTarget<'_>,
    denylist: &Denylist,
) -> Result<(), DenialReason> {
    let Some(def) = CAPABILITIES.iter().find(|def| def.name == name) else {
        return Err(DenialReason::UndeclaredCapability);
    };
    if !grants.declares(name) {
        return Err(DenialReason::UndeclaredCapability);
    }
    match def.scope {
        ScopeRequirement::None => {
            if cap_disabled(def) {
                return Err(DenialReason::CapDisabled);
            }
            Ok(())
        }
        ScopeRequirement::PathGlob => {
            let CheckTarget::Path(path) = target else {
                return Err(DenialReason::OutOfScope);
            };
            if !grants.path_matches(name, path) {
                return Err(DenialReason::OutOfScope);
            }
            if cap_disabled(def) {
                return Err(DenialReason::CapDisabled);
            }
            if denylist.denies_fs(path) {
                return Err(DenialReason::PolicyDenied);
            }
            Ok(())
        }
        ScopeRequirement::EnvKey => {
            let CheckTarget::Env(key) = target else {
                return Err(DenialReason::OutOfScope);
            };
            if !grants.env_matches(name, key) {
                return Err(DenialReason::OutOfScope);
            }
            if cap_disabled(def) {
                return Err(DenialReason::CapDisabled);
            }
            if denylist.denies_env(key) {
                return Err(DenialReason::PolicyDenied);
            }
            Ok(())
        }
    }
}

/// Reserved slot in the denial order. SH-3 never disables a declared capability.
fn cap_disabled(_def: &CapabilityDef) -> bool {
    false
}

fn split_grant(grant: &str) -> Result<(&str, Option<&str>), GrantError> {
    if grant.is_empty() || grant.contains('\0') {
        return Err(GrantError::Malformed(grant.to_string()));
    }
    let (name, scope) = match grant.split_once(':') {
        Some((name, scope)) => {
            if scope.is_empty() {
                return Err(GrantError::Malformed(grant.to_string()));
            }
            (name, Some(scope))
        }
        None => (grant, None),
    };
    if !valid_cap_name(name) {
        return Err(GrantError::Malformed(grant.to_string()));
    }
    Ok((name, scope))
}

fn valid_cap_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let (Some(first), Some(second)) = (parts.next(), parts.next()) else {
        return false;
    };
    if !valid_segment(first) || !valid_segment(second) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(third) => parts.next().is_none() && valid_segment(third),
    }
}

fn valid_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn parse_env_scope(scope: &str) -> Option<EnvPattern> {
    let wildcard = scope.ends_with('*');
    let core = if wildcard {
        scope.strip_suffix('*').unwrap_or(scope)
    } else {
        scope
    };
    let mut chars = core.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_uppercase() || ch == '_' => {}
        _ => return None,
    }
    if !chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_') {
        return None;
    }
    Some(EnvPattern {
        prefix: core.to_string(),
        wildcard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::resolve;

    #[test]
    fn unknown_and_malformed_grants_fail() {
        assert!(matches!(
            GrantSet::parse(&["sys.metrics.read"]),
            Err(GrantError::Unsupported(_))
        ));
        assert!(matches!(
            GrantSet::parse(&["proc.cmdline.read"]),
            Err(GrantError::Unsupported(_))
        ));
        assert!(matches!(
            GrantSet::parse(&["fs.read"]),
            Err(GrantError::Malformed(_))
        ));
        assert!(matches!(
            GrantSet::parse(&["sys.info.read:/etc"]),
            Err(GrantError::Malformed(_))
        ));
        assert!(matches!(
            GrantSet::parse(&["fs.read:relative"]),
            Err(GrantError::Malformed(_))
        ));
        assert!(matches!(
            GrantSet::parse(&["fs.read:/etc/passwd", "net.interfaces.read"]),
            Err(GrantError::Unsupported(_))
        ));
    }

    #[test]
    fn declared_shadow_is_policy_denied() {
        let grants = GrantSet::parse(&["fs.read:/etc/shadow"]).expect("grant");
        let resolved = resolve("/etc/shadow").expect("resolve");
        let err = check(
            &grants,
            "fs.read",
            CheckTarget::Path(&resolved.scope),
            &Denylist::new(None),
        )
        .expect_err("denied");
        assert_eq!(err, DenialReason::PolicyDenied);
    }

    #[test]
    fn env_prefix_and_sensitive_key() {
        let grants = GrantSet::parse(&["env.read:NOVBOT_*"]).expect("grant");
        assert!(matches!(
            check(
                &grants,
                "env.read",
                CheckTarget::Env("NOVBOT_TEST_TOKEN"),
                &Denylist::new(None),
            ),
            Err(DenialReason::PolicyDenied)
        ));
        assert!(matches!(
            check(
                &grants,
                "env.read",
                CheckTarget::Env("OTHER"),
                &Denylist::new(None),
            ),
            Err(DenialReason::OutOfScope)
        ));
        let exact = GrantSet::parse(&["env.read:PATH"]).expect("grant");
        assert!(check(
            &exact,
            "env.read",
            CheckTarget::Env("PATH"),
            &Denylist::new(None),
        )
        .is_ok());
    }
}
