// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Deny-all stubs for WASI filesystem, sockets, and environment.
//!
//! Clocks, random, and captured stdio stay on the real wasmtime-wasi host.
//! A stub records `undeclared_capability` and returns a guest error. It traps
//! only after the run has already recorded [`crate::DENIAL_LIMIT`] denials.

use crate::host::RunCtx;
use wasmtime::component::types::{ComponentInstance, ComponentItem, Type};
use wasmtime::component::{Component, Linker, LinkerInstance, Val};
use wasmtime::{Engine, Result};

/// Preview 2 package version registered by wasmtime-wasi 49.
///
/// The guest may import an older 0.2 patch. An exact linker entry at that
/// older version would hide the semver-compatible host. Stubs are installed
/// on this host version instead.
const HOST_WASI_P2: &str = "0.2.12";

pub(crate) fn install(
    linker: &mut Linker<RunCtx>,
    engine: &Engine,
    component: &Component,
) -> Result<()> {
    linker.allow_shadowing(true);
    let ty = component.component_type();
    let imports = ty
        .imports(engine)
        .map(|(name, item)| (name.to_string(), item))
        .collect::<Vec<_>>();
    for (name, item) in &imports {
        let Some(capability) = wasi_capability(name) else {
            continue;
        };
        let ComponentItem::ComponentInstance(instance) = &item.ty else {
            continue;
        };
        let host_name = host_instance_name(name);
        shadow_instance(linker, engine, &host_name, instance, capability)?;
    }
    Ok(())
}

fn host_instance_name(guest_import: &str) -> String {
    match guest_import.rsplit_once('@') {
        Some((path, _)) => format!("{path}@{HOST_WASI_P2}"),
        None => guest_import.to_string(),
    }
}

fn wasi_capability(import: &str) -> Option<&'static str> {
    if import.starts_with("wasi:filesystem/") {
        Some("wasi:filesystem")
    } else if import.starts_with("wasi:sockets/") {
        Some("wasi:sockets")
    } else if import.starts_with("wasi:cli/environment") {
        Some("wasi:environment")
    } else {
        None
    }
}

fn shadow_instance(
    linker: &mut Linker<RunCtx>,
    engine: &Engine,
    name: &str,
    instance: &ComponentInstance,
    capability: &'static str,
) -> Result<()> {
    let functions = instance
        .exports(engine)
        .filter_map(|(export, item)| match &item.ty {
            ComponentItem::ComponentFunc(_) => Some(export.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut opened = linker.root().into_instance(name)?;
    for function in functions {
        define_stub(&mut opened, &function, capability)?;
    }
    Ok(())
}

fn define_stub(
    inst: &mut LinkerInstance<'_, RunCtx>,
    name: &str,
    capability: &'static str,
) -> Result<()> {
    let capability = capability.to_string();
    inst.func_new(name, move |mut store, func_ty, args, results| {
        let target = first_string(args);
        store.data_mut().deny_wasi(&capability, target)?;
        fill_results(&func_ty, results)
    })?;
    Ok(())
}

fn fill_results(
    func: &wasmtime::component::types::ComponentFunc,
    results: &mut [Val],
) -> Result<()> {
    let types = func.results().collect::<Vec<_>>();
    if types.len() != results.len() {
        return Err(wasmtime::Error::msg("wasi stub result arity mismatch"));
    }
    for (ty, slot) in types.iter().zip(results.iter_mut()) {
        *slot = deny_val(ty)?;
    }
    Ok(())
}

fn deny_val(ty: &Type) -> Result<Val> {
    if let Type::Result(result) = ty {
        let err_val = match result.err() {
            Some(err_ty) => Some(Box::new(zero_val(&err_ty)?)),
            None => None,
        };
        return Ok(Val::Result(Err(err_val)));
    }
    zero_val(ty)
}

fn zero_val(ty: &Type) -> Result<Val> {
    Ok(match ty {
        Type::Bool => Val::Bool(false),
        Type::S8 => Val::S8(0),
        Type::U8 => Val::U8(0),
        Type::S16 => Val::S16(0),
        Type::U16 => Val::U16(0),
        Type::S32 => Val::S32(0),
        Type::U32 => Val::U32(0),
        Type::S64 => Val::S64(0),
        Type::U64 => Val::U64(0),
        Type::Float32 => Val::Float32(0.0),
        Type::Float64 => Val::Float64(0.0),
        Type::Char => Val::Char('\0'),
        Type::String => Val::String(String::new()),
        Type::List(_) => Val::List(Vec::new()),
        Type::Map(_) => Val::Map(Vec::new()),
        Type::Flags(_) => Val::Flags(Vec::new()),
        Type::Option(_) => Val::Option(None),
        Type::Record(record) => {
            let mut fields = Vec::new();
            for field in record.fields() {
                fields.push((field.name.to_string(), zero_val(&field.ty)?));
            }
            return Ok(Val::Record(fields));
        }
        Type::Tuple(tuple) => {
            let mut items = Vec::new();
            for field in tuple.types() {
                items.push(zero_val(&field)?);
            }
            return Ok(Val::Tuple(items));
        }
        Type::Enum(enum_ty) => {
            let name = enum_ty
                .names()
                .next()
                .ok_or_else(|| wasmtime::Error::msg("empty enum"))?;
            return Ok(Val::Enum(name.to_string()));
        }
        Type::Variant(variant) => {
            let case = variant
                .cases()
                .next()
                .ok_or_else(|| wasmtime::Error::msg("empty variant"))?;
            let payload = match case.ty {
                Some(payload) => Some(Box::new(zero_val(&payload)?)),
                None => None,
            };
            return Ok(Val::Variant(case.name.to_string(), payload));
        }
        Type::Result(_) => return deny_val(ty),
        Type::FixedLengthList(_)
        | Type::Own(_)
        | Type::Borrow(_)
        | Type::Future(_)
        | Type::Stream(_)
        | Type::ErrorContext => {
            return Err(wasmtime::Error::msg("cannot synthesize wasi value"));
        }
    })
}

fn first_string(values: &[Val]) -> String {
    for value in values {
        if let Some(text) = find_string(value) {
            if !text.is_empty() {
                return clip(&text, 512);
            }
        }
    }
    String::new()
}

fn find_string(value: &Val) -> Option<String> {
    match value {
        Val::String(text) => Some(text.clone()),
        Val::List(items) | Val::Tuple(items) => items.iter().find_map(find_string),
        Val::Record(fields) => fields.iter().find_map(|(_, item)| find_string(item)),
        Val::Option(Some(item)) | Val::Variant(_, Some(item)) => find_string(item),
        Val::Result(Ok(Some(item)) | Err(Some(item))) => find_string(item),
        _ => None,
    }
}

fn clip(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}
