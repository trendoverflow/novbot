// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Node tag format (T1.3) and the Kubernetes-style selector (T2.2).
//!
//! Operator and node-declared pairs are never rewritten. Only center-derived
//! `sys.*` values go through [`normalize_system_value`].

use std::collections::{BTreeMap, BTreeSet};

pub const MAX_CENTER_TAGS: usize = 32;
pub const MAX_NODE_TAGS: usize = 32;
pub const MAX_SELECTOR_CHARS: usize = 1024;
pub const MAX_REQUIREMENTS: usize = 16;
pub const MAX_SET_VALUES: usize = 32;
pub const MAX_TARGETS: usize = 5_000;
const MAX_TAG_CHARS: usize = 63;
const MAX_STORED_CHARS: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenterTagError {
    KeyInvalid,
    ValueInvalid,
    KeyReserved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredTag {
    pub key: String,
    pub value: String,
    pub valid: bool,
    pub invalid_reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requirement {
    Eq { key: String, value: String },
    NotEq { key: String, value: String },
    In { key: String, values: Vec<String> },
    NotIn { key: String, values: Vec<String> },
    Exists { key: String },
    NotExists { key: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub requirements: Vec<Requirement>,
    pub canonical: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorParse {
    /// Blank input. The node list treats this as no filter.
    Empty,
    /// Explicit `*`: every node.
    All,
    Filter(Selector),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorError {
    pub message: String,
    pub position: usize,
}

/// `sys` and `sys.*` are reserved for the system source.
pub fn is_reserved_key(key: &str) -> bool {
    key == "sys" || key.starts_with("sys.")
}

/// Validate a center-written pair. Uppercase is rejected, not lowercased.
pub fn validate_center_tag(key: &str, value: &str) -> Result<(), CenterTagError> {
    if !key_ok(key) {
        return Err(CenterTagError::KeyInvalid);
    }
    if is_reserved_key(key) {
        return Err(CenterTagError::KeyReserved);
    }
    if !value_ok(value) {
        return Err(CenterTagError::ValueInvalid);
    }
    Ok(())
}

/// Classify node-declared pairs. Invalid pairs are kept (cut at 255 chars).
///
/// Valid pairs past [`MAX_NODE_TAGS`], in key order, become `over_limit`.
pub fn classify_declared_tags(
    pairs: &std::collections::HashMap<String, String>,
) -> Vec<DeclaredTag> {
    let mut drafts: Vec<Draft> = pairs
        .iter()
        .map(|(key, value)| classify_one(key, value))
        .collect();
    drafts.sort_by(|a, b| a.sort_key.cmp(&b.sort_key).then(a.key.cmp(&b.key)));
    let mut valid_seen = 0usize;
    for draft in &mut drafts {
        if draft.valid {
            if valid_seen >= MAX_NODE_TAGS {
                draft.valid = false;
                draft.invalid_reason = Some("over_limit");
            } else {
                valid_seen += 1;
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(drafts.len());
    for draft in drafts {
        if !seen.insert(draft.key.clone()) {
            continue;
        }
        out.push(DeclaredTag {
            key: draft.key,
            value: draft.value,
            valid: draft.valid,
            invalid_reason: draft.invalid_reason,
        });
    }
    out
}

/// Lowercase, map characters outside the value set to `_`, cut to 63.
/// An empty input yields no tag.
pub fn normalize_system_value(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let mut out = String::new();
    for c in raw.chars().take(MAX_TAG_CHARS) {
        let c = if c.is_ascii_uppercase() {
            c.to_ascii_lowercase()
        } else {
            c
        };
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub fn parse_selector(input: &str) -> Result<SelectorParse, SelectorError> {
    if input.chars().count() > MAX_SELECTOR_CHARS {
        return Err(SelectorError {
            message: format!("selector exceeds {MAX_SELECTOR_CHARS} characters"),
            position: 0,
        });
    }
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(SelectorParse::Empty);
    }
    if trimmed == "*" {
        return Ok(SelectorParse::All);
    }
    let mut parser = Parser {
        input,
        i: trim_start_index(input),
    };
    let mut requirements = Vec::new();
    loop {
        if requirements.len() >= MAX_REQUIREMENTS {
            return Err(SelectorError {
                message: format!("selector exceeds {MAX_REQUIREMENTS} requirements"),
                position: parser.i,
            });
        }
        let start = parser.i;
        requirements.push(parser.requirement()?);
        parser.skip_ws();
        if parser.eof() {
            break;
        }
        if parser.eat(b',') {
            parser.skip_ws();
            if parser.eof() {
                return Err(parser.err("trailing comma"));
            }
            continue;
        }
        return Err(SelectorError {
            message: "expected ',' or end of selector".into(),
            position: start,
        });
    }
    let requirements = reject_conflicting_equality(requirements)?;
    let canonical = canonical_of(&requirements);
    Ok(SelectorParse::Filter(Selector {
        requirements,
        canonical,
    }))
}

pub fn selector_matches(selector: &Selector, tags: &BTreeMap<String, String>) -> bool {
    selector.requirements.iter().all(|req| match req {
        Requirement::Eq { key, value } => tags.get(key).map(String::as_str) == Some(value),
        Requirement::NotEq { key, value } => tags.get(key).map(String::as_str) != Some(value),
        Requirement::In { key, values } => tags
            .get(key)
            .is_some_and(|value| values.iter().any(|item| item == value)),
        Requirement::NotIn { key, values } => tags
            .get(key)
            .map(|value| values.iter().all(|item| item != value))
            .unwrap_or(true),
        Requirement::Exists { key } => tags.contains_key(key),
        Requirement::NotExists { key } => !tags.contains_key(key),
    })
}

struct Draft {
    sort_key: String,
    key: String,
    value: String,
    valid: bool,
    invalid_reason: Option<&'static str>,
}

fn classify_one(key: &str, value: &str) -> Draft {
    let stored_key = cut_chars(key, MAX_STORED_CHARS);
    let stored_value = cut_chars(value, MAX_STORED_CHARS);
    let reason = if char_len(key) > MAX_TAG_CHARS || char_len(value) > MAX_TAG_CHARS {
        Some("too_long")
    } else if !key_ok(key) {
        Some("key_invalid")
    } else if is_reserved_key(key) {
        Some("key_reserved")
    } else if value.is_empty() {
        Some("value_empty")
    } else if !value_ok(value) {
        Some("value_invalid")
    } else {
        None
    };
    Draft {
        sort_key: key.to_string(),
        key: stored_key,
        value: stored_value,
        valid: reason.is_none(),
        invalid_reason: reason,
    }
}

fn key_ok(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    let mut n = 1usize;
    for c in chars {
        n += 1;
        if n > MAX_TAG_CHARS || !is_tag_char(c) {
            return false;
        }
    }
    true
}

fn value_ok(value: &str) -> bool {
    let n = char_len(value);
    (1..=MAX_TAG_CHARS).contains(&n) && value.chars().all(is_tag_char)
}

fn is_tag_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn cut_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

struct ParsedReq {
    start: usize,
    req: Requirement,
}

struct Parser<'a> {
    input: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    fn eof(&self) -> bool {
        self.i >= self.input.len()
    }

    fn rest(&self) -> &'a str {
        &self.input[self.i..]
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if !c.is_ascii_whitespace() {
                break;
            }
            self.i += c.len_utf8();
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.rest().as_bytes().first() == Some(&byte) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn err(&self, message: impl Into<String>) -> SelectorError {
        SelectorError {
            message: message.into(),
            position: self.i,
        }
    }

    fn requirement(&mut self) -> Result<ParsedReq, SelectorError> {
        self.skip_ws();
        let start = self.i;
        if self.eof() {
            return Err(self.err("expected a requirement"));
        }
        if self.eat(b'!') {
            self.skip_ws();
            let key = self.key()?;
            return Ok(ParsedReq {
                start,
                req: Requirement::NotExists { key },
            });
        }
        let key = self.key()?;
        self.skip_ws();
        if self.eof() || self.rest().starts_with(',') {
            return Ok(ParsedReq {
                start,
                req: Requirement::Exists { key },
            });
        }
        if self.rest().starts_with("!=") {
            self.i += 2;
            self.skip_ws();
            let value = self.value()?;
            return Ok(ParsedReq {
                start,
                req: Requirement::NotEq { key, value },
            });
        }
        if self.eat(b'=') {
            self.skip_ws();
            let value = self.value()?;
            return Ok(ParsedReq {
                start,
                req: Requirement::Eq { key, value },
            });
        }
        if self.eat_word("notin") {
            let values = self.value_set()?;
            return Ok(ParsedReq {
                start,
                req: Requirement::NotIn { key, values },
            });
        }
        if self.eat_word("in") {
            let values = self.value_set()?;
            return Ok(ParsedReq {
                start,
                req: Requirement::In { key, values },
            });
        }
        Err(self.err("expected '=', '!=', 'in', 'notin', or end of requirement"))
    }

    fn eat_word(&mut self, word: &str) -> bool {
        let rest = self.rest();
        if !rest.starts_with(word) {
            return false;
        }
        let after = rest[word.len()..].chars().next();
        if after
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
        {
            return false;
        }
        self.i += word.len();
        true
    }

    fn key(&mut self) -> Result<String, SelectorError> {
        let start = self.i;
        let raw = self.token(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
        })?;
        if !key_ok(&raw) {
            return Err(SelectorError {
                message: format!("invalid selector key `{raw}`"),
                position: start,
            });
        }
        Ok(raw)
    }

    fn value(&mut self) -> Result<String, SelectorError> {
        let start = self.i;
        let raw = self.token(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
        })?;
        if !value_ok(&raw) {
            return Err(SelectorError {
                message: format!("invalid selector value `{raw}`"),
                position: start,
            });
        }
        Ok(raw)
    }

    fn token(&mut self, ok: impl Fn(char) -> bool) -> Result<String, SelectorError> {
        let start = self.i;
        let rest = self.rest();
        let mut end = 0usize;
        for c in rest.chars() {
            if !ok(c) {
                break;
            }
            end += c.len_utf8();
        }
        if end == 0 {
            return Err(self.err("expected a token"));
        }
        self.i += end;
        Ok(self.input[start..self.i].to_string())
    }

    fn value_set(&mut self) -> Result<Vec<String>, SelectorError> {
        self.skip_ws();
        if !self.eat(b'(') {
            return Err(self.err("expected '('"));
        }
        let mut values = Vec::new();
        loop {
            self.skip_ws();
            if values.len() >= MAX_SET_VALUES {
                return Err(self.err(format!("value set exceeds {MAX_SET_VALUES} values")));
            }
            values.push(self.value()?);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b')') {
                break;
            }
            return Err(self.err("expected ',' or ')'"));
        }
        if values.is_empty() {
            return Err(self.err("value set is empty"));
        }
        Ok(values)
    }
}

fn reject_conflicting_equality(reqs: Vec<ParsedReq>) -> Result<Vec<Requirement>, SelectorError> {
    let mut eq_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut eq_pos: BTreeMap<String, usize> = BTreeMap::new();
    for parsed in &reqs {
        if let Requirement::Eq { key, value } = &parsed.req {
            eq_values
                .entry(key.clone())
                .or_default()
                .push(value.clone());
            eq_pos.insert(key.clone(), parsed.start);
        }
    }
    for (key, values) in &eq_values {
        let mut distinct = values.clone();
        distinct.sort();
        distinct.dedup();
        if distinct.len() > 1 {
            return Err(SelectorError {
                message: format!(
                    "conflicting equality for `{key}`; use `{key} in ({})`",
                    distinct.join(",")
                ),
                position: eq_pos.get(key).copied().unwrap_or(0),
            });
        }
    }
    let mut out = Vec::new();
    let mut seen_eq: BTreeSet<String> = BTreeSet::new();
    for parsed in reqs {
        if let Requirement::Eq { key, .. } = &parsed.req {
            if !seen_eq.insert(key.clone()) {
                continue;
            }
        }
        out.push(parsed.req);
    }
    Ok(out)
}

fn canonical_of(reqs: &[Requirement]) -> String {
    let mut owned: Vec<Requirement> = reqs
        .iter()
        .map(|req| match req {
            Requirement::In { key, values } => Requirement::In {
                key: key.clone(),
                values: sorted_unique(values),
            },
            Requirement::NotIn { key, values } => Requirement::NotIn {
                key: key.clone(),
                values: sorted_unique(values),
            },
            other => other.clone(),
        })
        .collect();
    owned.sort_by(|a, b| req_sort_key(a).cmp(&req_sort_key(b)));
    owned
        .iter()
        .map(req_canonical)
        .collect::<Vec<_>>()
        .join(",")
}

fn sorted_unique(values: &[String]) -> Vec<String> {
    let mut values = values.to_vec();
    values.sort();
    values.dedup();
    values
}

fn req_sort_key(req: &Requirement) -> (String, u8, String) {
    match req {
        Requirement::Eq { key, value } => (key.clone(), 0, value.clone()),
        Requirement::NotEq { key, value } => (key.clone(), 1, value.clone()),
        Requirement::In { key, values } => (key.clone(), 2, values.join(",")),
        Requirement::NotIn { key, values } => (key.clone(), 3, values.join(",")),
        Requirement::Exists { key } => (key.clone(), 4, String::new()),
        Requirement::NotExists { key } => (key.clone(), 5, String::new()),
    }
}

fn req_canonical(req: &Requirement) -> String {
    match req {
        Requirement::Eq { key, value } => format!("{key}={value}"),
        Requirement::NotEq { key, value } => format!("{key}!={value}"),
        Requirement::In { key, values } => format!("{key} in ({})", values.join(",")),
        Requirement::NotIn { key, values } => format!("{key} notin ({})", values.join(",")),
        Requirement::Exists { key } => key.clone(),
        Requirement::NotExists { key } => format!("!{key}"),
    }
}

fn trim_start_index(input: &str) -> usize {
    input
        .find(|c: char| !c.is_ascii_whitespace())
        .unwrap_or(input.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn tags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn filter(raw: &str) -> Selector {
        match parse_selector(raw).unwrap() {
            SelectorParse::Filter(selector) => selector,
            other => panic!("expected filter, got {other:?}"),
        }
    }

    #[test]
    fn center_tag_rejects_uppercase_and_reserved_keys() {
        assert_eq!(
            validate_center_tag("Env", "prod"),
            Err(CenterTagError::KeyInvalid)
        );
        assert_eq!(
            validate_center_tag("env", "Prod"),
            Err(CenterTagError::ValueInvalid)
        );
        assert_eq!(
            validate_center_tag("sys", "x"),
            Err(CenterTagError::KeyReserved)
        );
        assert_eq!(
            validate_center_tag("sys.os", "linux"),
            Err(CenterTagError::KeyReserved)
        );
        assert!(validate_center_tag("env", "prod").is_ok());
        assert!(validate_center_tag("role", "db").is_ok());
    }

    #[test]
    fn declared_pairs_keep_invalid_text_and_do_not_lowercase() {
        let classified = classify_declared_tags(&map(&[
            ("Env", "Prod"),
            ("sys.os", "linux"),
            ("role", "db"),
        ]));
        let by_key: HashMap<_, _> = classified
            .iter()
            .map(|tag| (tag.key.as_str(), tag))
            .collect();
        // HashMap keeps one env; the cases below use separate maps.
        assert!(by_key["Env"].invalid_reason == Some("key_invalid"));
        assert!(!by_key["Env"].valid);
        assert_eq!(by_key["Env"].key, "Env");
        assert_eq!(by_key["sys.os"].invalid_reason, Some("key_reserved"));
        assert!(by_key["role"].valid);

        let value = classify_declared_tags(&map(&[("env", "Prod")]));
        assert_eq!(value[0].invalid_reason, Some("value_invalid"));
        assert_eq!(value[0].value, "Prod");

        let empty = classify_declared_tags(&map(&[("env", "")]));
        assert_eq!(empty[0].invalid_reason, Some("value_empty"));
    }

    #[test]
    fn over_limit_marks_the_33rd_valid_pair_in_key_order() {
        let mut pairs = HashMap::new();
        for i in 0..33 {
            pairs.insert(format!("k{i:02}"), "v".into());
        }
        let classified = classify_declared_tags(&pairs);
        let invalid: Vec<_> = classified.iter().filter(|tag| !tag.valid).collect();
        assert_eq!(invalid.len(), 1);
        assert_eq!(invalid[0].invalid_reason, Some("over_limit"));
        assert_eq!(invalid[0].key, "k32");
        assert_eq!(classified.iter().filter(|tag| tag.valid).count(), 32);
    }

    #[test]
    fn too_long_is_stored_cut_at_255() {
        let key = "a".repeat(70);
        let classified = classify_declared_tags(&map(&[(&key, "ok")]));
        assert_eq!(classified[0].invalid_reason, Some("too_long"));
        assert_eq!(classified[0].key.chars().count(), 70);
        let huge = "b".repeat(300);
        let classified = classify_declared_tags(&map(&[(&huge, "ok")]));
        assert_eq!(classified[0].key.chars().count(), 255);
    }

    #[test]
    fn system_value_normalization() {
        assert_eq!(
            normalize_system_value("0.3.0+git.ab12").as_deref(),
            Some("0.3.0_git.ab12")
        );
        assert_eq!(normalize_system_value("Linux").as_deref(), Some("linux"));
        assert_eq!(normalize_system_value("").as_deref(), None);
        assert_eq!(normalize_system_value("+++").as_deref(), Some("___"));
        let long = format!("v{}", "1".repeat(80));
        assert_eq!(normalize_system_value(&long).unwrap().chars().count(), 63);
    }

    #[test]
    fn selector_forms_and_canonical_order() {
        let selector = filter(" role=db , env=prod ");
        assert_eq!(selector.canonical, "env=prod,role=db");
        assert!(selector_matches(
            &selector,
            &tags(&[("role", "db"), ("env", "prod")])
        ));
        assert!(!selector_matches(
            &selector,
            &tags(&[("role", "web"), ("env", "prod")])
        ));

        let presence = filter("team");
        assert!(selector_matches(&presence, &tags(&[("team", "platform")])));
        assert!(!selector_matches(&presence, &tags(&[("role", "db")])));

        let missing = filter("!team");
        assert!(selector_matches(&missing, &tags(&[("role", "db")])));
        assert!(!selector_matches(&missing, &tags(&[("team", "a")])));

        let any = filter("env in (test, dev, dev)");
        assert_eq!(any.canonical, "env in (dev,test)");
        assert!(selector_matches(&any, &tags(&[("env", "dev")])));
        assert!(!selector_matches(&any, &tags(&[("env", "prod")])));
        assert!(!selector_matches(&any, &tags(&[])));

        let not_eq = filter("env!=prod");
        assert!(selector_matches(&not_eq, &tags(&[("env", "dev")])));
        assert!(selector_matches(&not_eq, &tags(&[])));
        assert!(!selector_matches(&not_eq, &tags(&[("env", "prod")])));

        let not_in = filter("env notin (dev,test)");
        assert!(selector_matches(&not_in, &tags(&[("env", "prod")])));
        assert!(selector_matches(&not_in, &tags(&[])));
        assert!(!selector_matches(&not_in, &tags(&[("env", "dev")])));
    }

    #[test]
    fn conflicting_equality_hints_at_in() {
        let err = parse_selector("env=dev,env=test").unwrap_err();
        assert!(err.message.contains("env in (dev,test)"), "{}", err.message);
        assert!(parse_selector("env=prod,env=prod").is_ok());
    }

    #[test]
    fn blank_and_star_are_not_filters() {
        assert_eq!(parse_selector("").unwrap(), SelectorParse::Empty);
        assert_eq!(parse_selector("  ").unwrap(), SelectorParse::Empty);
        assert_eq!(parse_selector("*").unwrap(), SelectorParse::All);
        assert_eq!(parse_selector(" * ").unwrap(), SelectorParse::All);
    }

    #[test]
    fn selector_rejects_garbage_without_panicking() {
        for raw in [
            "",
            "*",
            "Env=prod",
            "env=Prod",
            "env in ()",
            "env in (a,)",
            ",env",
            "!!!",
            "env=",
        ] {
            let _ = parse_selector(raw);
        }
        let mut junk = String::new();
        for i in 0..40 {
            junk.push_str(&format!("x{i}=v,"));
        }
        assert!(parse_selector(&junk).is_err());
    }
}
