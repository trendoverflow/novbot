// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

fn main() {
    println!("cargo:rerun-if-env-changed=NOVBOT_TEST_DATABASE_URL");
    println!("cargo:rustc-check-cfg=cfg(novbot_test_db_missing)");
    let url = std::env::var("NOVBOT_TEST_DATABASE_URL").unwrap_or_default();
    if url.trim().is_empty() {
        println!("cargo:rustc-cfg=novbot_test_db_missing");
    }
}
