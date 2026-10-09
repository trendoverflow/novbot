// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Typed wrappers over the `novbot:skill@1.0.0` host imports.

#[cfg(target_arch = "wasm32")]
use crate::{FileStat, HostError, ListenSocket};

pub mod fs {
    use crate::{FileStat, HostError};

    pub fn read(path: &str, max_bytes: u32) -> Result<Vec<u8>, HostError> {
        #[cfg(target_arch = "wasm32")]
        {
            return crate::guest_bind::novbot::skill::fs::read(path, max_bytes)
                .map_err(HostError::from);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::testing::call(|host| host.read(path, max_bytes))
        }
    }

    pub fn stat(path: &str) -> Result<FileStat, HostError> {
        #[cfg(target_arch = "wasm32")]
        {
            return crate::guest_bind::novbot::skill::fs::stat(path)
                .map(FileStat::from)
                .map_err(HostError::from);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::testing::call(|host| host.stat(path))
        }
    }

    pub fn list(dir: &str, max_depth: u8, max_entries: u32) -> Result<Vec<String>, HostError> {
        #[cfg(target_arch = "wasm32")]
        {
            return crate::guest_bind::novbot::skill::fs::list(dir, max_depth, max_entries)
                .map_err(HostError::from);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::testing::call(|host| host.list(dir, max_depth, max_entries))
        }
    }
}

pub mod net {
    use crate::{HostError, ListenSocket};

    pub fn listening_ports() -> Result<Vec<ListenSocket>, HostError> {
        #[cfg(target_arch = "wasm32")]
        {
            return crate::guest_bind::novbot::skill::net::listening_ports()
                .map(|sockets| sockets.into_iter().map(ListenSocket::from).collect())
                .map_err(HostError::from);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::testing::call(|host| host.listening_ports())
        }
    }
}

pub mod sys {
    use crate::HostError;

    pub fn info() -> Result<String, HostError> {
        #[cfg(target_arch = "wasm32")]
        {
            return crate::guest_bind::novbot::skill::sys::info().map_err(HostError::from);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::testing::call(|host| host.info())
        }
    }

    pub fn time_sync() -> Result<String, HostError> {
        #[cfg(target_arch = "wasm32")]
        {
            return crate::guest_bind::novbot::skill::sys::time_sync().map_err(HostError::from);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::testing::call(|host| host.time_sync())
        }
    }
}

pub mod env {
    use crate::HostError;

    pub fn get(key: &str) -> Result<Option<String>, HostError> {
        #[cfg(target_arch = "wasm32")]
        {
            return crate::guest_bind::novbot::skill::sys::env_get(key).map_err(HostError::from);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::testing::call(|host| host.env_get(key))
        }
    }
}

pub mod log {
    pub fn log(level: u8, message: &str) {
        #[cfg(target_arch = "wasm32")]
        {
            crate::guest_bind::novbot::skill::log::log(level, message);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::testing::log(level, message);
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl From<crate::guest_bind::novbot::skill::types::HostError> for HostError {
    fn from(err: crate::guest_bind::novbot::skill::types::HostError) -> Self {
        use crate::guest_bind::novbot::skill::types::HostError as Wit;
        match err {
            Wit::Denied(denial) => Self::Denied {
                capability: denial.capability,
                target: denial.target,
                reason: denial.reason,
            },
            Wit::NotFound(message) => Self::NotFound(message),
            Wit::Io(message) => Self::Io(message),
            Wit::LimitExceeded(message) => Self::LimitExceeded(message),
            Wit::Invalid(message) => Self::Invalid(message),
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl From<crate::guest_bind::novbot::skill::fs::FileStat> for FileStat {
    fn from(stat: crate::guest_bind::novbot::skill::fs::FileStat) -> Self {
        Self {
            exists: stat.exists,
            kind: stat.kind,
            mode: stat.mode,
            uid: stat.uid,
            gid: stat.gid,
            size: stat.size,
            mtime_unix_ms: stat.mtime_unix_ms,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl From<crate::guest_bind::novbot::skill::net::ListenSocket> for ListenSocket {
    fn from(socket: crate::guest_bind::novbot::skill::net::ListenSocket) -> Self {
        Self {
            proto: socket.proto,
            addr: socket.addr,
            port: socket.port,
            uid: socket.uid,
        }
    }
}
