// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Node-wide denylist applied after a path or env key is in scope.
//!
//! Filesystem hits use reason `policy_denied`. Sensitive environment keys use
//! the same predicate as `novbot_core::skill`'s `env_get` stub.

use crate::path::display_path;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Denylist {
    data_dirs: Vec<String>,
}

impl Denylist {
    pub fn new(data_dir: Option<&Path>) -> Self {
        let mut data_dirs = Vec::new();
        if let Some(dir) = data_dir {
            push_data_dir(&mut data_dirs, dir);
        }
        if let Some(env_dir) = std::env::var_os("NOVBOT_DATA_DIR") {
            push_data_dir(&mut data_dirs, Path::new(&env_dir));
        }
        Self { data_dirs }
    }

    pub fn denies_fs(&self, path: &Path) -> bool {
        let text = display_path(path);
        aliases(&text).iter().any(|alias| self.hit(alias))
    }

    /// Same sensitive-key rule as the in-process `env_get` skill.
    pub fn denies_env(&self, key: &str) -> bool {
        let lower = key.to_ascii_lowercase();
        lower.contains("secret")
            || lower.contains("password")
            || lower.contains("token")
            || lower.contains("api_key")
            || lower.ends_with("_key")
    }

    fn hit(&self, path: &str) -> bool {
        if static_deny(path) {
            return true;
        }
        self.data_dirs.iter().any(|dir| under_dir(path, dir))
    }
}

fn push_data_dir(dirs: &mut Vec<String>, dir: &Path) {
    let text = if let Ok(real) = std::fs::canonicalize(dir) {
        display_path(&real)
    } else {
        display_path(dir)
    };
    for alias in aliases(&text) {
        if !dirs.contains(&alias) {
            dirs.push(alias);
        }
    }
}

fn under_dir(path: &str, dir: &str) -> bool {
    path == dir || path.starts_with(&format!("{dir}/"))
}

fn aliases(path: &str) -> Vec<String> {
    let path = trim_slash(path);
    let mut out = vec![path.to_string()];
    if let Some(rest) = path.strip_prefix("/private/") {
        out.push(format!("/{rest}"));
    } else if let Some(rest) = path.strip_prefix('/') {
        out.push(format!("/private/{rest}"));
    }
    out
}

fn trim_slash(path: &str) -> &str {
    if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    }
}

fn static_deny(path: &str) -> bool {
    if path == "/etc/shadow" || path == "/etc/gshadow" {
        return true;
    }
    if let Some(name) = path.strip_prefix("/etc/ssh/") {
        if !name.contains('/') && name.starts_with("ssh_host_") && name.ends_with("_key") {
            return true;
        }
    }
    if path == "/etc/ssl/private" || path.starts_with("/etc/ssl/private/") {
        return true;
    }
    if path == "/root" || path.starts_with("/root/") {
        return true;
    }
    if home_ssh(path) {
        return true;
    }
    if proc_secret(path) {
        return true;
    }
    false
}

fn home_ssh(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/home/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let Some(user) = parts.next() else {
        return false;
    };
    if user.is_empty() {
        return false;
    }
    parts.next() == Some(".ssh")
}

fn proc_secret(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/proc/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let Some(pid) = parts.next() else {
        return false;
    };
    if pid.is_empty() {
        return false;
    }
    let Some(leaf) = parts.next() else {
        return false;
    };
    parts.next().is_none() && (leaf == "environ" || leaf == "mem")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path;

    #[test]
    fn static_paths_and_lookalikes() {
        let deny = Denylist::new(None);
        assert!(deny.denies_fs(Path::new("/etc/shadow")));
        assert!(deny.denies_fs(Path::new("/private/etc/shadow")));
        assert!(!deny.denies_fs(Path::new("/etc/shadow-backup")));
        assert!(deny.denies_fs(Path::new("/etc/ssh/ssh_host_ed25519_key")));
        assert!(!deny.denies_fs(Path::new("/etc/ssh/ssh_host_ed25519_key.pub")));
        assert!(deny.denies_fs(Path::new("/etc/ssl/private/key.pem")));
        assert!(!deny.denies_fs(Path::new("/etc/ssl/privateer")));
        assert!(deny.denies_fs(Path::new("/root/x")));
        assert!(!deny.denies_fs(Path::new("/rootkit")));
        assert!(deny.denies_fs(Path::new("/home/alice/.ssh/id_rsa")));
        assert!(!deny.denies_fs(Path::new("/home/alice/.ssh.bak")));
        assert!(deny.denies_fs(Path::new("/proc/self/environ")));
        assert!(deny.denies_fs(Path::new("/proc/1/mem")));
        assert!(!deny.denies_fs(Path::new("/proc/1/cmdline")));
    }

    #[test]
    fn data_dir_covers_its_tree() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, b"x").unwrap();
        let deny = Denylist::new(Some(dir.path()));
        let resolved = path::resolve(&file.display().to_string()).unwrap();
        assert!(deny.denies_fs(&resolved.own));
        assert!(deny.denies_fs(dir.path()));
        assert!(!deny.denies_env("PATH"));
        assert!(deny.denies_env("NOVBOT_TEST_TOKEN"));
        assert!(deny.denies_env("API_KEY"));
    }
}
