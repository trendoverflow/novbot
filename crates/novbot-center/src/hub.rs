// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Live Session fan-out: HTTP DispatchCommand can push to a connected node.

use novbot_proto::ServerMessage;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<RwLock<HashMap<String, mpsc::Sender<ServerMessage>>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, node_id: String, tx: mpsc::Sender<ServerMessage>) {
        self.write().insert(node_id, tx);
    }

    pub async fn unregister(&self, node_id: &str, tx: &mpsc::Sender<ServerMessage>) {
        let mut guard = self.write();
        if let Some(existing) = guard.get(node_id) {
            if existing.same_channel(tx) {
                guard.remove(node_id);
            }
        }
    }

    /// A live Session sender is registered for `node_id`.
    ///
    /// Desired-set handlers use this to choose `pending` or `queued`. They do not send.
    pub fn is_connected(&self, node_id: &str) -> bool {
        self.read().contains_key(node_id)
    }

    pub async fn send(&self, node_id: &str, msg: ServerMessage) -> bool {
        let tx = { self.read().get(node_id).cloned() };
        if let Some(tx) = tx {
            tx.send(msg).await.is_ok()
        } else {
            false
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, mpsc::Sender<ServerMessage>>> {
        self.inner.read().unwrap_or_else(|err| err.into_inner())
    }

    fn write(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<String, mpsc::Sender<ServerMessage>>> {
        self.inner.write().unwrap_or_else(|err| err.into_inner())
    }
}
