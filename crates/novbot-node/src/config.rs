// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Process configuration for the node daemon.
//!
//! `center_grpc`, `node_id`, and `bootstrap_token` resolve from the CLI flag,
//! then the environment, then a TOML file. `node_id`, if still unset, is loaded
//! from or generated under the data directory. Any other file key is rejected.
//! Specs and schedules come from the center, not from this file.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;
use tonic::transport::Uri;
use uuid::Uuid;

const ENV_CENTER_GRPC: &str = "NOVBOT_CENTER_GRPC";
const ENV_NODE_ID: &str = "NOVBOT_NODE_ID";
const ENV_BOOTSTRAP_TOKEN: &str = "NOVBOT_BOOTSTRAP_TOKEN";
const CENTER_EXAMPLE: &str = "http://novbot-center:50051";
const NODE_EXAMPLE: &str = "orb-arm-1";

/// Bootstrap token. `Debug` and `Display` never reveal the value.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl FromStr for Secret {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Secret(s.to_string()))
    }
}

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Cli,
    Env,
    File,
    DataDirPersisted,
    DataDirGenerated,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Source::Cli => "cli",
            Source::Env => "env",
            Source::File => "file",
            Source::DataDirPersisted => "data_dir(persisted)",
            Source::DataDirGenerated => "data_dir(generated)",
        })
    }
}

/// A value plus the source that won precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved<T> {
    pub value: T,
    pub source: Source,
}

/// Validated center gRPC address. `Display` prints the URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CenterAddr {
    pub uri: String,
    pub host: String,
    pub port: u16,
}

impl std::fmt::Display for CenterAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.uri)
    }
}

/// CLI inputs for the three file-overridable settings. Empty values are unset.
#[derive(Debug, Clone, Default)]
pub struct CliValues {
    pub center_grpc: Option<String>,
    pub node_id: Option<String>,
    pub bootstrap_token: Option<Secret>,
}

/// TOML file. Only these keys are accepted.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(default)]
    pub center_grpc: Option<String>,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub bootstrap_token: Option<Secret>,
}

/// Settings after precedence and address validation.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub center_grpc: Resolved<CenterAddr>,
    pub node_id: Resolved<String>,
    pub bootstrap_token: Option<Resolved<Secret>>,
    pub config_file: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{}", format_missing(.what, .flag, .env))]
    Missing {
        what: String,
        flag: String,
        env: String,
    },
    #[error(
        "invalid center gRPC address `{value}`: {reason}; example: {example}",
        example = CENTER_EXAMPLE
    )]
    InvalidCenterAddr { value: String, reason: String },
    #[error("config file {path}: {reason}")]
    ConfigFile { path: PathBuf, reason: String },
    #[error("node ID unavailable at {path}: {reason}\n{}", node_id_examples())]
    NodeIdUnavailable { path: PathBuf, reason: String },
}

/// Resolve `center_grpc` (required), `node_id`, and `bootstrap_token` (optional).
///
/// Precedence per value is CLI, then `env`, then `file`. When `node_id` is still
/// unset, it is loaded from or generated under `data_dir` (see
/// [`node_id_from_data_dir`]). Whitespace-only values are treated as unset.
/// `env` is a lookup such as `|key| std::env::var(key).ok()`.
///
/// A missing or invalid center address is returned before the data directory
/// is read or written.
pub fn resolve(
    cli: CliValues,
    env: impl Fn(&str) -> Option<String>,
    file: Option<(PathBuf, FileConfig)>,
    data_dir: &Path,
) -> Result<NodeConfig, ConfigError> {
    let (config_file, file_cfg) = match file {
        Some((path, cfg)) => (Some(path), Some(cfg)),
        None => (None, None),
    };
    let file_cfg = file_cfg.as_ref();

    let center_raw = pick(
        cli.center_grpc,
        env(ENV_CENTER_GRPC),
        file_cfg.and_then(|cfg| cfg.center_grpc.clone()),
    );
    let node_raw = pick(
        cli.node_id,
        env(ENV_NODE_ID),
        file_cfg.and_then(|cfg| cfg.node_id.clone()),
    );
    let token_raw = pick_secret(
        cli.bootstrap_token,
        env(ENV_BOOTSTRAP_TOKEN),
        file_cfg.and_then(|cfg| cfg.bootstrap_token.clone()),
    );

    let center_grpc = match center_raw {
        Some((value, source)) => Resolved {
            value: validate_center_addr(&value)?,
            source,
        },
        None => return Err(missing_center()),
    };
    let node_id = match node_raw {
        Some((value, source)) => Resolved { value, source },
        None => node_id_from_data_dir(data_dir)?,
    };

    Ok(NodeConfig {
        center_grpc,
        node_id,
        bootstrap_token: token_raw.map(|(value, source)| Resolved { value, source }),
        config_file,
    })
}

/// Load `<data_dir>/node_id`, or generate `node-<uuid>` and persist it.
///
/// An existing non-empty file is trimmed and returned as
/// [`Source::DataDirPersisted`]. A missing or whitespace-only file is replaced
/// with a new id ([`Source::DataDirGenerated`]). The data directory is created
/// only when a new id must be written. A read error on an existing file, or a
/// failure to create the directory or write the file, is
/// [`ConfigError::NodeIdUnavailable`].
pub fn node_id_from_data_dir(data_dir: &Path) -> Result<Resolved<String>, ConfigError> {
    let path = data_dir.join("node_id");
    if path.exists() {
        let text = std::fs::read_to_string(&path).map_err(|err| node_id_unavailable(&path, err))?;
        let id = text.trim();
        if !id.is_empty() {
            return Ok(Resolved {
                value: id.to_string(),
                source: Source::DataDirPersisted,
            });
        }
    }

    std::fs::create_dir_all(data_dir).map_err(|err| node_id_unavailable(&path, err))?;
    let id = format!("node-{}", Uuid::new_v4());
    std::fs::write(&path, &id).map_err(|err| node_id_unavailable(&path, err))?;
    Ok(Resolved {
        value: id,
        source: Source::DataDirGenerated,
    })
}

fn node_id_unavailable(path: &Path, err: std::io::Error) -> ConfigError {
    ConfigError::NodeIdUnavailable {
        path: path.to_path_buf(),
        reason: err.to_string(),
    }
}

/// Parse TOML that may contain only `center_grpc`, `node_id`, and `bootstrap_token`.
pub fn parse_file_config(path: &Path, text: &str) -> Result<FileConfig, ConfigError> {
    toml::from_str(text).map_err(|err| ConfigError::ConfigFile {
        path: path.to_path_buf(),
        reason: toml_reason(text, &err),
    })
}

/// Read and parse a config file. An I/O failure names `path` and the OS error.
pub fn load_file_config(path: &Path) -> Result<FileConfig, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|err| ConfigError::ConfigFile {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })?;
    parse_file_config(path, &text)
}

/// Require `http` or `https`, a host, and an explicit port in `1..=65535`.
///
/// The path must be empty or `/`. Query and fragment are rejected. IPv6 hosts
/// are stored without brackets so DNS lookup can use them.
pub fn validate_center_addr(raw: &str) -> Result<CenterAddr, ConfigError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(invalid_addr(value, "address is empty"));
    }

    let uri: Uri = match value.parse() {
        Ok(uri) => uri,
        Err(err) => {
            return Err(invalid_addr(value, format!("not a valid URI ({err})")));
        }
    };

    match uri.scheme_str() {
        Some("http" | "https") => {}
        Some(scheme) => {
            return Err(invalid_addr(
                value,
                format!("unsupported scheme `{scheme}`, expected http or https"),
            ));
        }
        None => {
            return Err(invalid_addr(
                value,
                "missing scheme, expected http or https",
            ));
        }
    }

    let Some(authority) = uri.authority().filter(|auth| !auth.as_str().is_empty()) else {
        return Err(invalid_addr(value, "missing host"));
    };
    let host = strip_ipv6_brackets(authority.host());
    if host.is_empty() {
        return Err(invalid_addr(value, "missing host"));
    }

    let port = match port_text(authority.as_str()) {
        Some(text) if !text.is_empty() => match text.parse::<u16>() {
            Ok(0) => return Err(invalid_addr(value, "port must be in 1..=65535")),
            Ok(port) => port,
            Err(_) => {
                return Err(invalid_addr(
                    value,
                    format!("invalid port `{text}`, expected a number in 1..=65535"),
                ));
            }
        },
        _ => return Err(invalid_addr(value, "missing explicit port")),
    };

    let path = uri.path();
    if !path.is_empty() && path != "/" {
        return Err(invalid_addr(value, format!("path `{path}` is not allowed")));
    }
    if uri.query().is_some() {
        return Err(invalid_addr(value, "query string is not allowed"));
    }
    if value.contains('#') {
        return Err(invalid_addr(value, "fragment is not allowed"));
    }

    Ok(CenterAddr {
        uri: value.to_string(),
        host,
        port,
    })
}

fn invalid_addr(value: &str, reason: impl Into<String>) -> ConfigError {
    ConfigError::InvalidCenterAddr {
        value: value.to_string(),
        reason: reason.into(),
    }
}

fn strip_ipv6_brackets(host: &str) -> String {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
        .to_string()
}

/// Port substring of an authority, without treating IPv6 colons as the port.
fn port_text(authority: &str) -> Option<&str> {
    let host_port = match authority.rsplit_once('@') {
        Some((_, host_port)) => host_port,
        None => authority,
    };
    if let Some(rest) = host_port.strip_prefix('[') {
        let end = rest.find(']')?;
        return rest[end + 1..].strip_prefix(':');
    }
    host_port.split_once(':').map(|(_, port)| port)
}

fn pick(
    cli: Option<String>,
    env_value: Option<String>,
    file_value: Option<String>,
) -> Option<(String, Source)> {
    if let Some(value) = nonempty(cli) {
        return Some((value, Source::Cli));
    }
    if let Some(value) = nonempty(env_value) {
        return Some((value, Source::Env));
    }
    nonempty(file_value).map(|value| (value, Source::File))
}

fn pick_secret(
    cli: Option<Secret>,
    env_value: Option<String>,
    file_value: Option<Secret>,
) -> Option<(Secret, Source)> {
    pick(
        cli.map(|secret| secret.expose().to_string()),
        env_value,
        file_value.map(|secret| secret.expose().to_string()),
    )
    .map(|(value, source)| (Secret(value), source))
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn missing_center() -> ConfigError {
    ConfigError::Missing {
        what: "center gRPC address".to_string(),
        flag: "--center-grpc".to_string(),
        env: ENV_CENTER_GRPC.to_string(),
    }
}

fn format_missing(what: &str, flag: &str, env: &str) -> String {
    format!(
        "missing {what}: set one of\n  {flag} {CENTER_EXAMPLE}\n  {env}={CENTER_EXAMPLE}\n  center_grpc = \"{CENTER_EXAMPLE}\"   (in the file passed via --config / NOVBOT_CONFIG)"
    )
}

fn node_id_examples() -> String {
    format!(
        "missing node ID: set one of\n  --node-id {NODE_EXAMPLE}\n  {ENV_NODE_ID}={NODE_EXAMPLE}\n  node_id = \"{NODE_EXAMPLE}\"   (in the file passed via --config / NOVBOT_CONFIG)"
    )
}

/// `toml::de::Error`'s Display repeats the offending source line, which may
/// contain the bootstrap token. Use only `message()` plus a line and column.
fn toml_reason(text: &str, err: &toml::de::Error) -> String {
    let message = err.message().trim().replace('\n', "; ");
    match err.span() {
        Some(span) => {
            let (line, column) = line_col(text, span.start);
            format!("{message} at line {line} column {column}")
        }
        None => message.to_string(),
    }
}

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut column = 1usize;
    for (index, ch) in text.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .copied()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn parsed(text: &str) -> FileConfig {
        parse_file_config(Path::new("node.toml"), text).unwrap()
    }

    fn file_pair(text: &str) -> Option<(PathBuf, FileConfig)> {
        Some((PathBuf::from("node.toml"), parsed(text)))
    }

    fn cli(center: &str, node: &str, token: &str) -> CliValues {
        CliValues {
            center_grpc: Some(center.to_string()),
            node_id: Some(node.to_string()),
            bootstrap_token: Some(token.parse().unwrap()),
        }
    }

    /// Tests that already supply a node id never touch the data directory.
    fn resolve(
        cli: CliValues,
        env: impl Fn(&str) -> Option<String>,
        file: Option<(PathBuf, FileConfig)>,
    ) -> Result<NodeConfig, ConfigError> {
        let dir = tempfile::tempdir().unwrap();
        super::resolve(cli, env, file, dir.path())
    }

    fn center_cli() -> CliValues {
        CliValues {
            center_grpc: Some("http://127.0.0.1:50051".into()),
            ..CliValues::default()
        }
    }

    fn assert_generated_id(id: &str) {
        let uuid = id
            .strip_prefix("node-")
            .unwrap_or_else(|| panic!("expected node-<uuid>, got {id}"));
        uuid::Uuid::parse_str(uuid).unwrap_or_else(|err| panic!("{id}: {err}"));
    }

    const FILE_TEXT: &str = "\
center_grpc = \"http://file.example:50051\"
node_id = \"file-node\"
bootstrap_token = \"file-token\"
";

    #[test]
    fn cli_beats_env_and_file_for_each_key() {
        let cfg = resolve(
            cli("http://cli.example:50051", "cli-node", "cli-token"),
            env_of(&[
                ("NOVBOT_CENTER_GRPC", "http://env.example:50051"),
                ("NOVBOT_NODE_ID", "env-node"),
                ("NOVBOT_BOOTSTRAP_TOKEN", "env-token"),
            ]),
            file_pair(FILE_TEXT),
        )
        .unwrap();

        assert_eq!(cfg.center_grpc.source, Source::Cli);
        assert_eq!(cfg.center_grpc.value.uri, "http://cli.example:50051");
        assert_eq!(cfg.center_grpc.value.host, "cli.example");
        assert_eq!(cfg.center_grpc.value.port, 50051);
        assert_eq!(cfg.node_id.source, Source::Cli);
        assert_eq!(cfg.node_id.value, "cli-node");
        let token = cfg.bootstrap_token.unwrap();
        assert_eq!(token.source, Source::Cli);
        assert_eq!(token.value.expose(), "cli-token");
        assert_eq!(cfg.config_file.as_deref(), Some(Path::new("node.toml")));
    }

    #[test]
    fn env_beats_file_for_each_key() {
        let cfg = resolve(
            CliValues::default(),
            env_of(&[
                ("NOVBOT_CENTER_GRPC", "http://env.example:50051"),
                ("NOVBOT_NODE_ID", "env-node"),
                ("NOVBOT_BOOTSTRAP_TOKEN", "env-token"),
            ]),
            file_pair(FILE_TEXT),
        )
        .unwrap();

        assert_eq!(cfg.center_grpc.source, Source::Env);
        assert_eq!(cfg.center_grpc.value.host, "env.example");
        assert_eq!(cfg.node_id.source, Source::Env);
        assert_eq!(cfg.node_id.value, "env-node");
        let token = cfg.bootstrap_token.unwrap();
        assert_eq!(token.source, Source::Env);
        assert_eq!(token.value.expose(), "env-token");
    }

    #[test]
    fn file_only_reports_file_source() {
        let cfg = resolve(CliValues::default(), env_of(&[]), file_pair(FILE_TEXT)).unwrap();
        assert_eq!(cfg.center_grpc.source, Source::File);
        assert_eq!(cfg.center_grpc.value.uri, "http://file.example:50051");
        assert_eq!(cfg.node_id.source, Source::File);
        assert_eq!(cfg.node_id.value, "file-node");
        let token = cfg.bootstrap_token.unwrap();
        assert_eq!(token.source, Source::File);
        assert_eq!(token.value.expose(), "file-token");
    }

    #[test]
    fn env_only_reports_env_source() {
        let cfg = resolve(
            CliValues::default(),
            env_of(&[
                ("NOVBOT_CENTER_GRPC", "http://env.example:443"),
                ("NOVBOT_NODE_ID", "env-node"),
                ("NOVBOT_BOOTSTRAP_TOKEN", "env-token"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.center_grpc.source, Source::Env);
        assert_eq!(cfg.center_grpc.value.port, 443);
        assert_eq!(cfg.node_id.source, Source::Env);
        assert_eq!(cfg.node_id.value, "env-node");
        assert_eq!(cfg.bootstrap_token.unwrap().value.expose(), "env-token");
        assert!(cfg.config_file.is_none());
    }

    #[test]
    fn each_key_keeps_its_own_source() {
        let cfg = resolve(
            CliValues {
                center_grpc: Some("http://cli.example:50051".into()),
                node_id: None,
                bootstrap_token: None,
            },
            env_of(&[("NOVBOT_NODE_ID", "env-node")]),
            file_pair(FILE_TEXT),
        )
        .unwrap();
        assert_eq!(cfg.center_grpc.source, Source::Cli);
        assert_eq!(cfg.node_id.source, Source::Env);
        assert_eq!(cfg.node_id.value, "env-node");
        assert_eq!(cfg.bootstrap_token.unwrap().source, Source::File);
    }

    #[test]
    fn empty_and_whitespace_values_are_unset() {
        let cfg = resolve(
            CliValues {
                center_grpc: Some("  ".into()),
                node_id: Some("".into()),
                bootstrap_token: Some("".parse().unwrap()),
            },
            env_of(&[
                ("NOVBOT_CENTER_GRPC", ""),
                ("NOVBOT_NODE_ID", "   "),
                ("NOVBOT_BOOTSTRAP_TOKEN", " \t "),
            ]),
            file_pair(
                "\
center_grpc = \"  http://file.example:50051  \"
node_id = \" file-node \"
bootstrap_token = \" file-token \"
",
            ),
        )
        .unwrap();
        assert_eq!(cfg.center_grpc.source, Source::File);
        assert_eq!(cfg.center_grpc.value.uri, "http://file.example:50051");
        assert_eq!(cfg.node_id.source, Source::File);
        assert_eq!(cfg.node_id.value, "file-node");
        assert_eq!(cfg.bootstrap_token.unwrap().value.expose(), "file-token");

        let cfg = resolve(
            CliValues {
                center_grpc: Some("".into()),
                node_id: None,
                bootstrap_token: Some("   ".parse().unwrap()),
            },
            env_of(&[
                ("NOVBOT_CENTER_GRPC", " http://env.example:50051 "),
                ("NOVBOT_NODE_ID", " env-node "),
                ("NOVBOT_BOOTSTRAP_TOKEN", ""),
            ]),
            file_pair("bootstrap_token = \"\"\n"),
        )
        .unwrap();
        assert_eq!(cfg.center_grpc.source, Source::Env);
        assert_eq!(cfg.center_grpc.value.host, "env.example");
        assert_eq!(cfg.node_id.source, Source::Env);
        assert_eq!(cfg.node_id.value, "env-node");
        assert!(cfg.bootstrap_token.is_none());
    }

    #[test]
    fn missing_center_names_flag_env_and_toml() {
        let err = resolve(
            CliValues {
                node_id: Some("orb-arm-1".into()),
                ..CliValues::default()
            },
            env_of(&[]),
            None,
        )
        .unwrap_err();
        assert!(matches!(
            &err,
            ConfigError::Missing { what, flag, env }
                if what == "center gRPC address"
                    && flag == "--center-grpc"
                    && env == "NOVBOT_CENTER_GRPC"
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("--center-grpc http://novbot-center:50051"),
            "{msg}"
        );
        assert!(
            msg.contains("NOVBOT_CENTER_GRPC=http://novbot-center:50051"),
            "{msg}"
        );
        assert!(
            msg.contains("center_grpc = \"http://novbot-center:50051\""),
            "{msg}"
        );
        assert!(!msg.contains("node_id"), "{msg}");
    }

    #[test]
    fn data_dir_source_display() {
        assert_eq!(Source::DataDirPersisted.to_string(), "data_dir(persisted)");
        assert_eq!(Source::DataDirGenerated.to_string(), "data_dir(generated)");
    }

    #[test]
    fn persisted_node_id_is_loaded_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node_id");
        std::fs::write(&path, "  persisted-node \n").unwrap();

        let cfg = super::resolve(center_cli(), env_of(&[]), None, dir.path()).unwrap();
        assert_eq!(cfg.node_id.source, Source::DataDirPersisted);
        assert_eq!(cfg.node_id.value, "persisted-node");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "  persisted-node \n"
        );
    }

    #[test]
    fn missing_node_id_is_generated_then_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node_id");
        let cfg = super::resolve(center_cli(), env_of(&[]), None, dir.path()).unwrap();
        assert_eq!(cfg.node_id.source, Source::DataDirGenerated);
        assert_generated_id(&cfg.node_id.value);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), cfg.node_id.value);

        let again = super::resolve(center_cli(), env_of(&[]), None, dir.path()).unwrap();
        assert_eq!(again.node_id.source, Source::DataDirPersisted);
        assert_eq!(again.node_id.value, cfg.node_id.value);
    }

    #[test]
    fn empty_or_whitespace_persisted_node_id_is_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node_id");
        for contents in ["", " \n\t "] {
            std::fs::write(&path, contents).unwrap();
            let cfg = super::resolve(center_cli(), env_of(&[]), None, dir.path()).unwrap();
            assert_eq!(cfg.node_id.source, Source::DataDirGenerated, "{contents:?}");
            assert_generated_id(&cfg.node_id.value);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), cfg.node_id.value);
        }
    }

    #[test]
    fn cli_env_and_file_win_over_persisted_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node_id");
        std::fs::write(&path, "persisted-node").unwrap();

        let cfg = super::resolve(
            CliValues::default(),
            env_of(&[]),
            file_pair("center_grpc = \"http://127.0.0.1:50051\"\nnode_id = \"file-node\"\n"),
            dir.path(),
        )
        .unwrap();
        assert_eq!(cfg.node_id.source, Source::File);
        assert_eq!(cfg.node_id.value, "file-node");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "persisted-node");

        let cfg = super::resolve(
            CliValues::default(),
            env_of(&[
                ("NOVBOT_CENTER_GRPC", "http://127.0.0.1:50051"),
                ("NOVBOT_NODE_ID", "env-node"),
            ]),
            file_pair("center_grpc = \"http://file.example:50051\"\nnode_id = \"file-node\"\n"),
            dir.path(),
        )
        .unwrap();
        assert_eq!(cfg.node_id.source, Source::Env);
        assert_eq!(cfg.node_id.value, "env-node");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "persisted-node");

        let cfg = super::resolve(
            CliValues {
                center_grpc: Some("http://127.0.0.1:50051".into()),
                node_id: Some("cli-node".into()),
                bootstrap_token: None,
            },
            env_of(&[("NOVBOT_NODE_ID", "env-node")]),
            file_pair("center_grpc = \"http://file.example:50051\"\nnode_id = \"file-node\"\n"),
            dir.path(),
        )
        .unwrap();
        assert_eq!(cfg.node_id.source, Source::Cli);
        assert_eq!(cfg.node_id.value, "cli-node");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "persisted-node");
    }

    #[test]
    fn unwritable_data_dir_reports_node_id_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("regular-file");
        std::fs::write(&blocker, b"x").unwrap();
        let data_dir = blocker.join("data");

        let err = super::resolve(center_cli(), env_of(&[]), None, &data_dir).unwrap_err();
        let ConfigError::NodeIdUnavailable { path, reason } = &err else {
            panic!("{err}");
        };
        let msg = err.to_string();
        assert!(msg.contains(&path.display().to_string()), "{msg}");
        assert!(msg.contains(reason.as_str()), "{msg}");
        assert!(msg.contains(&data_dir.display().to_string()), "{msg}");
        assert!(msg.contains("--node-id orb-arm-1"), "{msg}");
        assert!(msg.contains("NOVBOT_NODE_ID=orb-arm-1"), "{msg}");
        assert!(msg.contains("node_id = \"orb-arm-1\""), "{msg}");
    }

    #[test]
    fn unreadable_persisted_node_id_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node_id");
        std::fs::create_dir(&path).unwrap();

        let err = super::resolve(center_cli(), env_of(&[]), None, dir.path()).unwrap_err();
        let ConfigError::NodeIdUnavailable {
            path: err_path,
            reason,
        } = &err
        else {
            panic!("{err}");
        };
        let msg = err.to_string();
        assert_eq!(err_path, &path);
        assert!(msg.contains(&path.display().to_string()), "{msg}");
        assert!(msg.contains(reason.as_str()), "{msg}");
        assert!(msg.contains("--node-id orb-arm-1"), "{msg}");
        assert!(msg.contains("NOVBOT_NODE_ID=orb-arm-1"), "{msg}");
        assert!(msg.contains("node_id = \"orb-arm-1\""), "{msg}");
    }

    #[test]
    fn missing_center_does_not_create_node_id_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = super::resolve(CliValues::default(), env_of(&[]), None, dir.path()).unwrap_err();
        assert!(matches!(
            &err,
            ConfigError::Missing { what, flag, env }
                if what == "center gRPC address"
                    && flag == "--center-grpc"
                    && env == "NOVBOT_CENTER_GRPC"
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("--center-grpc http://novbot-center:50051"),
            "{msg}"
        );
        assert!(
            msg.contains("NOVBOT_CENTER_GRPC=http://novbot-center:50051"),
            "{msg}"
        );
        assert!(
            msg.contains("center_grpc = \"http://novbot-center:50051\""),
            "{msg}"
        );
        assert!(!msg.contains("node_id"), "{msg}");
        assert!(!msg.contains("--node-id"), "{msg}");
        assert!(!dir.path().join("node_id").exists());
    }

    #[test]
    fn invalid_center_does_not_create_node_id_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = super::resolve(
            CliValues {
                center_grpc: Some("http://host".into()),
                ..CliValues::default()
            },
            env_of(&[]),
            None,
            dir.path(),
        )
        .unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidCenterAddr { .. }),
            "{err}"
        );
        assert!(!dir.path().join("node_id").exists());
    }

    #[test]
    fn unknown_config_keys_are_rejected() {
        let path = Path::new("/etc/novbot/node.toml");
        for (text, key) in [
            ("specs = []\n", "specs"),
            ("schedules = []\n", "schedules"),
            ("labels = \"x\"\n", "labels"),
        ] {
            let err = parse_file_config(path, text).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains(key), "{msg}");
            assert!(msg.contains(path.to_str().unwrap()), "{msg}");
            assert!(matches!(err, ConfigError::ConfigFile { .. }), "{msg}");
        }
    }

    #[test]
    fn malformed_toml_does_not_include_token() {
        let text = "bootstrap_token = s3cr3t-tok\n";
        let err = parse_file_config(Path::new("node.toml"), text).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("s3cr3t-tok"), "{msg}");
        assert!(!msg.contains(text.trim()), "{msg}");
        assert!(msg.contains("node.toml"), "{msg}");
    }

    #[test]
    fn accepts_center_addresses() {
        let cases = [
            ("http://127.0.0.1:50051", "127.0.0.1", 50051),
            ("https://center.example.com:443", "center.example.com", 443),
            (
                "http://novbot-ce-arm.orb.local:50051",
                "novbot-ce-arm.orb.local",
                50051,
            ),
            ("http://[::1]:50051", "::1", 50051),
            ("http://127.0.0.1:50051/", "127.0.0.1", 50051),
        ];
        for (raw, host, port) in cases {
            let addr = validate_center_addr(raw).unwrap();
            assert_eq!(addr.host, host, "{raw}");
            assert_eq!(addr.port, port, "{raw}");
            assert_eq!(addr.uri, raw);
            assert_eq!(addr.to_string(), raw);
        }
    }

    #[test]
    fn rejects_center_addresses() {
        let cases = [
            "novbot-ce-arm.orb.local:50051",
            "http://host:notaport",
            "http://host",
            "ftp://host:1",
            "http://:50051",
            "http://host:0",
            "http://host:70000",
            "http://host:50051/x",
            "http://host:50051?x=1",
            "http://host:50051#frag",
        ];
        for raw in cases {
            let err = validate_center_addr(raw).unwrap_err();
            let msg = err.to_string();
            assert!(
                matches!(err, ConfigError::InvalidCenterAddr { .. }),
                "{msg}"
            );
            assert!(msg.contains(raw), "{msg}");
            assert!(msg.contains(CENTER_EXAMPLE), "{msg}");
            assert!(msg.contains("example"), "{msg}");
        }
    }

    #[test]
    fn invalid_address_from_any_source_fails_resolve() {
        let bad = "novbot-ce-arm.orb.local:50051";
        let file = parsed(&format!(
            "center_grpc = \"{bad}\"\nnode_id = \"file-node\"\n"
        ));
        let err = resolve(
            CliValues::default(),
            env_of(&[]),
            Some((PathBuf::from("node.toml"), file)),
        )
        .unwrap_err();
        assert!(err.to_string().contains(bad), "{err}");

        let err = resolve(
            CliValues::default(),
            env_of(&[("NOVBOT_CENTER_GRPC", bad), ("NOVBOT_NODE_ID", "n1")]),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidCenterAddr { .. }),
            "{err}"
        );

        let err = resolve(
            CliValues {
                center_grpc: Some(bad.into()),
                node_id: Some("n1".into()),
                bootstrap_token: None,
            },
            env_of(&[("NOVBOT_CENTER_GRPC", "http://127.0.0.1:50051")]),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains(bad), "{err}");
    }

    #[test]
    fn token_is_redacted_in_debug_and_display() {
        let token = "s3cr3t-tok";
        let secret: Secret = token.parse().unwrap();
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(secret.to_string(), "<redacted>");
        assert_eq!(secret.expose(), token);

        let file = parsed(&format!(
            "center_grpc = \"http://127.0.0.1:50051\"\nnode_id = \"n1\"\nbootstrap_token = \"{token}\"\n"
        ));
        let file_dbg = format!("{file:?}");
        assert!(!file_dbg.contains(token), "{file_dbg}");
        assert!(file_dbg.contains("<redacted>"), "{file_dbg}");

        let cfg = resolve(
            CliValues::default(),
            env_of(&[]),
            Some((PathBuf::from("node.toml"), file)),
        )
        .unwrap();
        let cfg_dbg = format!("{cfg:?}");
        assert!(!cfg_dbg.contains(token), "{cfg_dbg}");
        assert!(cfg_dbg.contains("<redacted>"), "{cfg_dbg}");
        assert_eq!(cfg.bootstrap_token.unwrap().value.expose(), token);
    }

    #[test]
    fn load_missing_file_names_path_and_io_error() {
        let path = Path::new("/no/such/novbot-node-config.toml");
        let err = load_file_config(path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/no/such/novbot-node-config.toml"), "{msg}");
        assert!(
            msg.contains("No such file") || msg.contains("os error"),
            "{msg}"
        );
    }
}
