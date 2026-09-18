// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

mod db;
mod grpc;
mod http;

use anyhow::Context;
use clap::Parser;
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "novbot-center", about = "NovBot API-only control center")]
struct Args {
    /// MySQL URL, e.g. mysql://novbot:novbot@127.0.0.1:3306/novbot
    #[arg(long, env = "NOVBOT_DATABASE_URL")]
    database_url: String,

    #[arg(long, env = "NOVBOT_GRPC_ADDR", default_value = "0.0.0.0:50051")]
    grpc_addr: SocketAddr,

    #[arg(long, env = "NOVBOT_HTTP_ADDR", default_value = "0.0.0.0:8080")]
    http_addr: SocketAddr,

    /// Optional shared bootstrap token required on Register.
    #[arg(long, env = "NOVBOT_BOOTSTRAP_TOKEN")]
    bootstrap_token: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let args = Args::parse();
    let db = db::Db::connect(&args.database_url)
        .await
        .context("database")?;
    db.migrate().await.context("migrate")?;

    let db_grpc = db.clone();
    let db_http = db;
    let token = args.bootstrap_token.clone();
    let grpc_addr = args.grpc_addr;
    let http_addr = args.http_addr;

    let grpc = tokio::spawn(async move {
        if let Err(e) = grpc::serve(grpc_addr, db_grpc, token).await {
            tracing::error!(error = %e, "gRPC server exited");
        }
    });
    let http = tokio::spawn(async move {
        if let Err(e) = http::serve(http_addr, db_http).await {
            tracing::error!(error = %e, "HTTP server exited");
        }
    });

    tokio::select! {
        _ = grpc => {},
        _ = http => {},
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown signal");
        }
    }
    Ok(())
}
