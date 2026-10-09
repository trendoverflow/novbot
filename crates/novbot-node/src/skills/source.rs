// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Package bytes. The only network path is gRPC `FetchArtifact`.

use async_trait::async_trait;
use novbot_proto::control_client::ControlClient;
use novbot_proto::FetchArtifactRequest;
use tokio_stream::StreamExt;

#[derive(Debug, Clone)]
pub struct Fetched {
    pub total_size: u64,
    pub data: Vec<u8>,
}

#[async_trait]
pub trait ArtifactSource: Send + Sync {
    /// Bytes of the package beginning at `offset`.
    ///
    /// `fetch_ticket` authorizes this sha256 only. Implementations must not
    /// include the ticket in errors or logs.
    async fn fetch(&self, sha256: &str, fetch_ticket: &str, offset: u64)
        -> Result<Fetched, String>;
}

/// Client for the center's server-streaming `FetchArtifact` RPC.
pub struct GrpcArtifactSource {
    endpoint: String,
    node_id: String,
}

impl GrpcArtifactSource {
    pub fn new(endpoint: impl Into<String>, node_id: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            node_id: node_id.into(),
        }
    }
}

#[async_trait]
impl ArtifactSource for GrpcArtifactSource {
    async fn fetch(
        &self,
        sha256: &str,
        fetch_ticket: &str,
        offset: u64,
    ) -> Result<Fetched, String> {
        let mut client = ControlClient::connect(self.endpoint.clone())
            .await
            .map_err(|err| format!("fetch connect failed: {err}"))?;
        let request = FetchArtifactRequest {
            node_id: self.node_id.clone(),
            sha256: sha256.to_string(),
            fetch_ticket: fetch_ticket.to_string(),
            offset: i64::try_from(offset).unwrap_or(i64::MAX),
        };
        let mut stream = client
            .fetch_artifact(request)
            .await
            .map_err(|err| redact_status(&err, fetch_ticket))?
            .into_inner();
        let mut data = Vec::new();
        let mut total_size = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| redact_status(&err, fetch_ticket))?;
            if chunk.offset < 0 || chunk.total_size < 0 {
                return Err("fetch response has a negative size".into());
            }
            total_size = chunk.total_size as u64;
            let expected = offset + data.len() as u64;
            if chunk.offset as u64 != expected {
                return Err("fetch response offset does not continue the download".into());
            }
            data.extend_from_slice(&chunk.data);
        }
        Ok(Fetched { total_size, data })
    }
}

fn redact_status(err: &tonic::Status, ticket: &str) -> String {
    let message = err.message();
    let message = if ticket.is_empty() {
        message.to_string()
    } else {
        message.replace(ticket, "[redacted]")
    };
    format!("fetch failed ({})", message)
}
