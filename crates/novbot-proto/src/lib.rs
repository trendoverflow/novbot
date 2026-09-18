// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

pub mod novbot {
    pub mod v1 {
        tonic::include_proto!("novbot.v1");
    }
}

pub use novbot::v1::*;
