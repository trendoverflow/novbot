// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

fn main() {
    if let Err(err) = novbot_skill::run() {
        eprintln!("novbot-skill: {err}");
        std::process::exit(1);
    }
}
