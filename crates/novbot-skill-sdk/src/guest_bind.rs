// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Guest bindings for `novbot:skill@1.0.0` world `skill`.
//!
//! Generated from `crates/novbot-skill-runtime/wit/skill.wit`. This is not a
//! second WIT definition.

#![allow(clippy::all)]

wit_bindgen::generate!({
    world: "skill",
    path: "../novbot-skill-runtime/wit",
    pub_export_macro: true,
    export_macro_name: "export_skill",
    default_bindings_module: "novbot_skill_sdk::guest_bind",
});
