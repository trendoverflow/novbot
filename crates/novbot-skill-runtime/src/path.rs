// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Path globs and canonicalisation.
//!
//! Scope matching uses the canonical path. `..` is resolved lexically.
//! Symlinks in ancestor directories are followed. The final component is not
//! followed when the path itself is opened.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Parent realpath joined with the final component. The final symlink is kept.
    pub own: PathBuf,
    /// Path used for `fs.read` / `fs.list` scope: the symlink target when the
    /// final component is a symlink, otherwise `own`.
    pub scope: PathBuf,
    pub final_symlink: bool,
    /// The final component is a symlink whose target could not be resolved.
    pub broken_symlink: bool,
}

pub fn reject_syntax(raw: &str) -> bool {
    raw.is_empty()
        || !raw.starts_with('/')
        || raw.contains('\0')
        || raw.contains('~')
        || raw.contains('\\')
}

pub fn valid_path_glob(scope: &str) -> bool {
    if reject_syntax(scope) {
        return false;
    }
    if scope.contains('{')
        || scope.contains('}')
        || scope.contains('[')
        || scope.contains(']')
        || scope.contains('$')
        || scope.contains("//")
    {
        return false;
    }
    let trimmed = scope.trim_end_matches('/');
    let body = if trimmed.is_empty() { "/" } else { trimmed };
    if body == "/" {
        return true;
    }
    for segment in body.split('/').skip(1) {
        if segment.is_empty() || segment == "." || segment == ".." {
            return false;
        }
        if segment.contains("**") && segment != "**" {
            return false;
        }
    }
    true
}

pub fn resolve(raw: &str) -> Option<Resolved> {
    if reject_syntax(raw) {
        return None;
    }
    let lexical = lexical_normalize(raw).ok()?;
    let own = canonical_own(&lexical);
    let final_symlink = fs::symlink_metadata(&own)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false);
    if !final_symlink {
        return Some(Resolved {
            scope: own.clone(),
            own,
            final_symlink: false,
            broken_symlink: false,
        });
    }
    match fs::read_link(&own) {
        Ok(target) => {
            let parent = own.parent().unwrap_or(Path::new("/"));
            let absolute = if target.is_absolute() {
                target
            } else {
                parent.join(target)
            };
            match follow_target(&absolute) {
                Ok(scope) => Some(Resolved {
                    own,
                    scope,
                    final_symlink: true,
                    broken_symlink: false,
                }),
                Err(()) => Some(Resolved {
                    scope: own.clone(),
                    own,
                    final_symlink: true,
                    broken_symlink: true,
                }),
            }
        }
        Err(_) => Some(Resolved {
            scope: own.clone(),
            own,
            final_symlink: true,
            broken_symlink: true,
        }),
    }
}

/// Match `candidate`, which is already canonical, against a grant glob.
pub fn glob_matches(pattern: &str, candidate: &Path) -> bool {
    let candidate = display_path(candidate);
    if !has_meta(pattern) {
        return canonical_literal(pattern).is_some_and(|real| real == candidate);
    }
    segment_glob(&canonical_pattern(pattern), &candidate)
}

pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    trim_trailing_slash(&text).to_string()
}

fn canonical_literal(pattern: &str) -> Option<String> {
    let resolved = resolve(pattern)?;
    Some(display_path(&resolved.own))
}

fn canonical_pattern(pattern: &str) -> String {
    let segments = segments(pattern);
    let glob_at = segments
        .iter()
        .position(|segment| segment.contains('*') || segment.contains('?'))
        .unwrap_or(segments.len());
    let prefix = if glob_at == 0 {
        "/".to_string()
    } else {
        format!("/{}", segments[..glob_at].join("/"))
    };
    let prefix_real = canonical_literal(&prefix).unwrap_or(prefix);
    let mut out = trim_trailing_slash(&prefix_real).to_string();
    if out.is_empty() {
        out = "/".to_string();
    }
    for segment in &segments[glob_at..] {
        if out == "/" {
            out = format!("/{segment}");
        } else {
            out.push('/');
            out.push_str(segment);
        }
    }
    out
}

fn canonical_own(lexical: &Path) -> PathBuf {
    if lexical == Path::new("/") {
        return PathBuf::from("/");
    }
    let parent = lexical.parent().unwrap_or(Path::new("/"));
    let parent_real = canonicalize_existing_prefix(parent);
    match lexical.file_name() {
        Some(name) => parent_real.join(name),
        None => parent_real,
    }
}

fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    let mut suffix = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if let Ok(real) = fs::canonicalize(&current) {
            let mut out = real;
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        if current.parent().is_none() || current == Path::new("/") {
            let mut out = PathBuf::from("/");
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        let Some(name) = current.file_name().map(|name| name.to_os_string()) else {
            return path.to_path_buf();
        };
        suffix.push(name);
        current = current.parent().unwrap().to_path_buf();
    }
}

fn follow_target(path: &Path) -> Result<PathBuf, ()> {
    if let Ok(real) = fs::canonicalize(path) {
        return Ok(real);
    }
    let raw = path.to_str().ok_or(())?;
    if reject_syntax(raw) && !raw.starts_with('/') {
        return Err(());
    }
    let lexical = lexical_normalize(raw)?;
    let own = canonical_own(&lexical);
    if fs::symlink_metadata(&own)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(());
    }
    Ok(own)
}

fn lexical_normalize(raw: &str) -> Result<PathBuf, ()> {
    if !raw.starts_with('/') {
        return Err(());
    }
    let mut parts = Vec::new();
    for part in raw.split('/').skip(1) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop();
            continue;
        }
        if part.contains('\0') || part.contains('~') || part.contains('\\') {
            return Err(());
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Ok(PathBuf::from("/"));
    }
    Ok(PathBuf::from(format!("/{}", parts.join("/"))))
}

fn has_meta(pattern: &str) -> bool {
    pattern.chars().any(|ch| ch == '*' || ch == '?')
}

fn segment_glob(pattern: &str, path: &str) -> bool {
    let pattern = segments(pattern);
    let path = segments(path);
    segments_match(&pattern, &path)
}

fn segments(path: &str) -> Vec<&str> {
    let trimmed = trim_trailing_slash(path);
    if trimmed.is_empty() || trimmed == "/" {
        return Vec::new();
    }
    trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn segments_match(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        if pattern.len() == 1 {
            return true;
        }
        return (0..=path.len()).any(|index| segments_match(&pattern[1..], &path[index..]));
    }
    if path.is_empty() || !segment_match(pattern[0], path[0]) {
        return false;
    }
    segments_match(&pattern[1..], &path[1..])
}

fn segment_match(pattern: &str, segment: &str) -> bool {
    fn rec(pattern: &[u8], segment: &[u8]) -> bool {
        if pattern.is_empty() {
            return segment.is_empty();
        }
        if pattern[0] == b'*' {
            return rec(&pattern[1..], segment)
                || (!segment.is_empty() && rec(pattern, &segment[1..]));
        }
        if segment.is_empty() {
            return false;
        }
        if pattern[0] == b'?' || pattern[0] == segment[0] {
            return rec(&pattern[1..], &segment[1..]);
        }
        false
    }
    rec(pattern.as_bytes(), segment.as_bytes())
}

fn trim_trailing_slash(path: &str) -> &str {
    if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn glob_stars_stay_inside_a_segment() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("base");
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("a.txt"), b"a").unwrap();
        std::fs::write(base.join("sub").join("b.txt"), b"b").unwrap();
        let star = format!("{}/*", base.display());
        let deep = format!("{}/**", base.display());
        assert!(glob_matches(&star, &resolve_own(&base.join("a.txt"))));
        assert!(!glob_matches(
            &star,
            &resolve_own(&base.join("sub").join("b.txt"))
        ));
        assert!(glob_matches(
            &deep,
            &resolve_own(&base.join("sub").join("b.txt"))
        ));
        assert!(glob_matches(&deep, &resolve_own(&base)));
    }

    #[test]
    fn dotdot_resolves_before_scope_match() {
        let root = tempfile::tempdir().unwrap();
        let grant = root.path().join("grant");
        std::fs::create_dir_all(grant.join("sub")).unwrap();
        let outside = root.path().join("outside.txt");
        std::fs::write(&outside, b"x").unwrap();
        let raw = format!("{}/sub/../../outside.txt", grant.display());
        let resolved = resolve(&raw).unwrap();
        let pattern = format!("{}/**", grant.display());
        assert!(!glob_matches(&pattern, &resolved.scope));
        assert_eq!(
            display_path(&resolved.scope),
            display_path(&resolve_own(&outside))
        );
    }

    #[test]
    fn symlink_scope_is_the_target() {
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("in");
        std::fs::create_dir_all(&inside).unwrap();
        let outside = root.path().join("outside.txt");
        std::fs::write(&outside, b"secret").unwrap();
        symlink(&outside, inside.join("link")).unwrap();
        let resolved = resolve(&inside.join("link").display().to_string()).unwrap();
        assert!(resolved.final_symlink);
        assert!(!resolved.broken_symlink);
        let pattern = format!("{}/**", inside.display());
        assert!(glob_matches(&pattern, &resolved.own));
        assert!(!glob_matches(&pattern, &resolved.scope));
    }

    fn resolve_own(path: &Path) -> PathBuf {
        resolve(&path.display().to_string()).unwrap().own
    }
}
