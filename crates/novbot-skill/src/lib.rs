// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `novbot-skill` builds, tests, packs, and publishes skill components.
//!
//! `test` runs a component in-process. It does not install the skill on a node.

mod archive;
mod inspect;
mod manifest;
mod project;
mod publish;
mod scaffold;

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "novbot-skill",
    version,
    about = "Build, test, pack, and publish NovBot skills"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a wasm32-wasip2 skill package.
    New {
        /// Skill name. Lowercase letters, digits, and hyphens.
        name: String,
    },
    /// Compile the skill to a wasm component (`module.wasm`).
    Build,
    /// Run `module.wasm` and print the runtime JSON, including denials.
    Test {
        /// JSON object passed to `run`. Defaults to `{}`.
        #[arg(long)]
        params: Option<String>,
        /// Fixture directory staged for this run.
        ///
        /// The copy is not passed as `RunRequest.data_dir`. That field is a
        /// denylist, so using it as a fixture root would report `policy_denied`
        /// for an in-scope read.
        #[arg(long)]
        fixture_root: Option<PathBuf>,
    },
    /// Write `[files]` hashes and a deterministic `<name>-<version>.nbskill`.
    Pack,
    /// Print the manifest, composed grants, and imported host interfaces.
    Inspect {
        /// Path to a `.nbskill` archive.
        file: PathBuf,
    },
    /// POST one `.nbskill`, or each `*.nbskill` in a directory.
    Publish {
        /// Package file or directory of `.nbskill` files.
        path: PathBuf,
        /// Center origin, for example `http://127.0.0.1:8080`.
        #[arg(long)]
        center: String,
        /// Bearer token. When omitted, `NOVBOT_API_TOKEN` is read. Never printed.
        #[arg(long)]
        token: Option<String>,
    },
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().context("current directory")?;
    match cli.command {
        Command::New { name } => scaffold::write(&cwd, &name),
        Command::Build => {
            let path = project::build(&cwd)?;
            println!("{}", path.display());
            Ok(())
        }
        Command::Test {
            params,
            fixture_root,
        } => {
            let output = project::test_project(
                &cwd,
                params.as_deref().unwrap_or("{}"),
                fixture_root.as_deref(),
            )?;
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(())
        }
        Command::Pack => {
            let packed = project::pack(&cwd)?;
            println!("{}@{}", packed.name, packed.version);
            println!("{}", packed.sha256);
            Ok(())
        }
        Command::Inspect { file } => {
            print!("{}", inspect::inspect(&file)?);
            Ok(())
        }
        Command::Publish {
            path,
            center,
            token,
        } => publish::publish(&path, &center, token),
    }
}
