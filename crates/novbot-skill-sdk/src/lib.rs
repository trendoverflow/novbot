// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Guest SDK for a `novbot:skill@1.0.0` component.
//!
//! Host imports are `fs.read`, `fs.stat`, `fs.list`, `net.listening-ports`,
//! `net.interfaces`, `sys.info`, `sys.time-sync`, `sys.env-get`, `sys.metrics`,
//! `proc.processes`, and `log`. Skills declare `fs.read`, `fs.stat`, `fs.list`,
//! `net.listening_ports.read`, `net.interfaces.read`, `sys.info.read`,
//! `sys.time_sync.read`, `sys.metrics.read`, `proc.list.read`, and `env.read`.
//! Bindings come from the runtime WIT.

mod error;
mod host;
mod types;

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub mod guest_bind;

#[cfg(not(target_arch = "wasm32"))]
pub mod testing;

pub use error::HostError;
pub use host::{env, fs, log, net, proc, sys};
pub use types::{findings_json, FileStat, Finding, FindingStatus, ListenSocket};

#[cfg(target_arch = "wasm32")]
pub use guest_bind::Guest;

/// Parse params JSON, run `check`, and serialize its value.
///
/// An empty parameter string is `{}`.
pub fn call_json<T, F>(params_json: &str, check: F) -> Result<String, String>
where
    T: serde::Serialize,
    F: FnOnce(serde_json::Value) -> Result<T, String>,
{
    let params = if params_json.trim().is_empty() {
        serde_json::Value::Object(Default::default())
    } else {
        serde_json::from_str(params_json).map_err(|err| format!("params json: {err}"))?
    };
    let value = check(params)?;
    serde_json::to_string(&value).map_err(|err| err.to_string())
}

/// Export `check` as the component `run` function.
///
/// `check` accepts a JSON value and returns a serializable JSON value. Invoke
/// this from a `wasm32-wasip2` cdylib. Host tests call `check` directly.
#[macro_export]
macro_rules! main {
    ($check:path) => {
        struct __NovbotSkillGuest;

        impl $crate::Guest for __NovbotSkillGuest {
            fn run(params_json: String) -> Result<String, String> {
                $crate::call_json(&params_json, $check)
            }
        }

        $crate::guest_bind::export_skill!(__NovbotSkillGuest);
    };
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::{with_host, MockHost};

    #[test]
    fn finding_json_has_t46_fields() {
        let finding = Finding::new("ssh.permit_root", FindingStatus::Fail, "high")
            .evidence(serde_json::json!({"value": "yes"}))
            .remediation("Set PermitRootLogin no.");
        let value: serde_json::Value = serde_json::from_str(&finding.to_json().unwrap()).unwrap();
        assert_eq!(value["id"], "ssh.permit_root");
        assert_eq!(value["status"], "fail");
        assert_eq!(value["severity"], "high");
        assert_eq!(value["evidence"]["value"], "yes");
        assert_eq!(value["remediation"], "Set PermitRootLogin no.");
        let array =
            findings_json(&[finding, Finding::new("other", FindingStatus::Pass, "low")]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&array).unwrap();
        assert_eq!(parsed[1]["status"], "pass");
    }

    #[test]
    fn mock_host_backs_imports() {
        let mut host = MockHost::new();
        host.insert_file("/etc/os-release", b"ID=demo\nVERSION_ID=1\n");
        host.deny("fs.read", "/etc/shadow", "out_of_scope");
        host.ports.push(ListenSocket {
            proto: "tcp".into(),
            addr: "127.0.0.1".into(),
            port: 22,
            uid: 0,
        });
        let ((), host) = with_host(host, || {
            let bytes = fs::read("/etc/os-release", 4).unwrap();
            assert_eq!(bytes, b"ID=d");
            let err = fs::read("/etc/shadow", 8).unwrap_err();
            assert!(matches!(
                err,
                HostError::Denied {
                    ref capability,
                    ref target,
                    ref reason,
                } if capability == "fs.read" && target == "/etc/shadow" && reason == "out_of_scope"
            ));
            let sockets = net::listening_ports().unwrap();
            assert_eq!(sockets[0].port, 22);
            log::log(1, "hello");
            let info = sys::info().unwrap();
            assert!(info.contains("mock-host"));
            assert!(env::get("PATH").unwrap().is_none());
            let metrics = sys::metrics().unwrap();
            assert!(metrics.contains("usage_percent"));
            let processes = proc::processes().unwrap();
            assert!(processes.contains("processes"));
            let interfaces = net::interfaces().unwrap();
            assert!(interfaces.contains("interfaces"));
        });
        assert!(host
            .calls
            .iter()
            .any(|call| call == "fs.read /etc/os-release"));
        assert_eq!(host.logs, vec![(1, "hello".to_string())]);
    }

    #[test]
    fn call_json_parses_params() {
        let text = call_json(r#"{"mode":"undeclared"}"#, |params| {
            Ok::<_, String>(params["mode"].clone())
        })
        .unwrap();
        assert_eq!(text, "\"undeclared\"");
        let empty = call_json("", Ok::<_, String>).unwrap();
        assert_eq!(empty, "{}");
    }
}
