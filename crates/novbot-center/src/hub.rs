// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Live Session fan-out: HTTP DispatchCommand can push to a connected node.
//!
//! Fetch tickets live here so HTTP handlers and the gRPC session share them
//! without an extra `AppState` field.

use novbot_proto::ServerMessage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const TICKET_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
struct TicketRec {
    node_id: String,
    sha256: String,
    expires: Instant,
}

#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<RwLock<HashMap<String, mpsc::Sender<ServerMessage>>>>,
    abi: Arc<RwLock<HashMap<String, String>>>,
    tickets: Arc<Mutex<HashMap<String, TicketRec>>>,
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

    /// Remember the ABI from Register. Empty means the node has no Skills Hub.
    pub fn note_abi(&self, node_id: &str, skill_abi: &str) {
        self.abi
            .write()
            .unwrap_or_else(|err| err.into_inner())
            .insert(node_id.to_string(), skill_abi.to_string());
    }

    pub fn skill_abi(&self, node_id: &str) -> Option<String> {
        self.abi
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .get(node_id)
            .cloned()
    }

    /// Random 128-bit ticket bound to `(node_id, sha256)`. Valid for 15 minutes.
    pub fn issue_ticket(&self, node_id: &str, sha256: &str) -> String {
        self.issue_ticket_ttl(node_id, sha256, TICKET_TTL)
    }

    pub fn issue_ticket_ttl(&self, node_id: &str, sha256: &str, ttl: Duration) -> String {
        let ticket = uuid::Uuid::new_v4().simple().to_string();
        let mut guard = self.tickets.lock().unwrap_or_else(|err| err.into_inner());
        let now = Instant::now();
        guard.retain(|_, rec| rec.expires > now);
        guard.insert(
            ticket.clone(),
            TicketRec {
                node_id: node_id.to_string(),
                sha256: sha256.trim().to_ascii_lowercase(),
                expires: now + ttl,
            },
        );
        ticket
    }

    /// `Ok` when the ticket is live and bound to this node and package.
    ///
    /// The error is a reason word (`bad`, `expired`, `mismatch`). It is not the ticket.
    pub fn check_ticket(
        &self,
        ticket: &str,
        node_id: &str,
        sha256: &str,
    ) -> Result<(), &'static str> {
        let mut guard = self.tickets.lock().unwrap_or_else(|err| err.into_inner());
        let Some(rec) = guard.get(ticket) else {
            return Err("bad");
        };
        if Instant::now() >= rec.expires {
            guard.remove(ticket);
            return Err("expired");
        }
        let sha = sha256.trim().to_ascii_lowercase();
        if rec.node_id != node_id || rec.sha256 != sha {
            return Err("mismatch");
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_is_single_package_and_expires() {
        let hub = Hub::new();
        let ticket = hub.issue_ticket("node-a", "abc");
        assert!(hub.check_ticket(&ticket, "node-a", "abc").is_ok());
        assert_eq!(hub.check_ticket("missing", "node-a", "abc"), Err("bad"));
        assert_eq!(hub.check_ticket(&ticket, "node-b", "abc"), Err("mismatch"));
        assert_eq!(hub.check_ticket(&ticket, "node-a", "def"), Err("mismatch"));
        let expired = hub.issue_ticket_ttl("node-a", "abc", Duration::ZERO);
        assert_eq!(hub.check_ticket(&expired, "node-a", "abc"), Err("expired"));
    }

    #[test]
    fn empty_abi_is_remembered() {
        let hub = Hub::new();
        assert!(hub.skill_abi("n").is_none());
        hub.note_abi("n", "");
        assert_eq!(hub.skill_abi("n").as_deref(), Some(""));
        hub.note_abi("n", "novbot:skill@1");
        assert_eq!(hub.skill_abi("n").as_deref(), Some("novbot:skill@1"));
    }
}
