// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Deterministic `.nbskill` gzip tar.
//!
//! Entry names are sorted. File headers use mtime 0, uid 0, gid 0, and mode
//! 0644. The gzip header mtime is 0. `skill.toml` is stored and is not part of
//! the hashed `[files]` map the caller passes in.

use flate2::{Compression, GzBuilder};
use std::collections::BTreeMap;
use std::io::{self, Write};
use tar::{Builder, Header};

enum Item<'a> {
    Dir,
    File(&'a [u8]),
}

pub fn pack(manifest: &str, files: &BTreeMap<String, Vec<u8>>) -> io::Result<Vec<u8>> {
    let mut entries: Vec<(&str, Item<'_>)> = Vec::new();
    entries.push(("skill.toml", Item::File(manifest.as_bytes())));
    if files.keys().any(|path| path.starts_with("schema/")) {
        entries.push(("schema", Item::Dir));
    }
    for (path, data) in files {
        entries.push((path.as_str(), Item::File(data.as_slice())));
    }
    entries.sort_by(|left, right| left.0.cmp(right.0));

    let mut tar_bytes = Vec::new();
    {
        let mut builder = Builder::new(&mut tar_bytes);
        for (path, item) in &entries {
            match item {
                Item::Dir => append_dir(&mut builder, path)?,
                Item::File(data) => append_file(&mut builder, path, data)?,
            }
        }
        builder.finish()?;
    }
    gzip(&tar_bytes)
}

fn append_dir<W: Write>(builder: &mut Builder<W>, path: &str) -> io::Result<()> {
    let mut header = Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o755);
    header.set_size(0);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder.append_data(&mut header, path, std::io::empty())
}

fn append_file<W: Write>(builder: &mut Builder<W>, path: &str, data: &[u8]) -> io::Result<()> {
    let mut header = Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_size(data.len() as u64);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder.append_data(&mut header, path, data)
}

fn gzip(tar_bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::default());
    encoder.write_all(tar_bytes)?;
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Cursor;

    #[test]
    fn archive_is_sorted_deterministic_and_normalized() {
        let mut files = BTreeMap::new();
        files.insert(
            "schema/params.json".into(),
            br#"{"type":"object"}"#.to_vec(),
        );
        files.insert("module.wasm".into(), b"\0asm".to_vec());
        files.insert("README.md".into(), b"hello\n".to_vec());
        let once = pack("schema_version = 1\n", &files).unwrap();
        let twice = pack("schema_version = 1\n", &files).unwrap();
        assert_eq!(once, twice);
        assert_eq!(&once[..4], b"\x1f\x8b\x08\x00");
        assert_eq!(&once[4..8], &[0, 0, 0, 0]);

        let mut archive = tar::Archive::new(GzDecoder::new(Cursor::new(once)));
        let mut names = Vec::new();
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let header = entry.header();
            let name = entry
                .path()
                .unwrap()
                .to_string_lossy()
                .trim_end_matches('/')
                .to_string();
            assert_eq!(header.mtime().unwrap(), 0);
            assert_eq!(header.uid().unwrap(), 0);
            assert_eq!(header.gid().unwrap(), 0);
            let mode = header.mode().unwrap() & 0o777;
            if name == "schema" {
                assert_eq!(mode, 0o755);
                assert!(header.entry_type().is_dir());
            } else {
                assert_eq!(mode, 0o644);
                assert!(header.entry_type().is_file());
            }
            names.push(name);
        }
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert_eq!(
            names,
            vec![
                "README.md".to_string(),
                "module.wasm".to_string(),
                "schema".to_string(),
                "schema/params.json".to_string(),
                "skill.toml".to_string(),
            ]
        );
    }
}
