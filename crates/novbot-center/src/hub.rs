// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Live Session fan-out: HTTP DispatchCommand can push to a connected node.

use novbot_proto::ServerMessage;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<RwLock<HashMap<String, mpsc::Sender<ServerMessage>>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, node_id: String, tx: mpsc::Sender<ServerMessage>) {
        self.inner.write().await.insert(node_id, tx);
    }

    pub async fn unregister(&self, node_id: &str, tx: &mpsc::Sender<ServerMessage>) {
        let mut guard = self.inner.write().await;
        if let Some(existing) = guard.get(node_id) {
            if existing.same_channel(tx) {
                guard.remove(node_id);
            }
        }
    }

    pub async fn send(&self, node_id: &str, msg: ServerMessage) -> bool {
        let tx = { self.inner.read().await.get(node_id).cloned() };
        if let Some(tx) = tx {
            tx.send(msg).await.is_ok()
        } else {
            false
        }
    }
}
