/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::Result;
use configmodel::Config;
use dag::ops::IdConvert;
use metalog::MetaLog;
use refencode::decode_bookmarks;
use refencode::decode_remotenames;
use treestate::treestate::TreeState;
use types::HgId;

use crate::errors::RevsetLookupError;

struct LookupArgs<'a> {
    change_id: &'a str,
    id_map: &'a dyn IdConvert,
    metalog: &'a MetaLog,
    treestate: Option<&'a TreeState>,
    config: &'a dyn Config,
}

pub fn resolve_single(
    config: &dyn Config,
    change_id: &str,
    id_map: &dyn IdConvert,
    metalog: &MetaLog,
    treestate: Option<&TreeState>,
) -> Result<HgId> {
    let args = LookupArgs {
        config,
        change_id,
        id_map,
        metalog,
        treestate,
    };
    let fns = [
        resolve_special,
        resolve_dot,
        resolve_bookmark,
        resolve_hash_prefix,
    ];

    for f in fns.iter() {
        if let Some(r) = f(&args)? {
            return Ok(r);
        }
    }

    Err(RevsetLookupError::RevsetNotFound(change_id.to_owned()).into())
}

fn resolve_special(args: &LookupArgs) -> Result<Option<HgId>> {
    if args.change_id == "null" {
        return Ok(Some(HgId::null_id().clone()));
    }
    if args.change_id != "tip" {
        return Ok(None);
    }
    args.metalog
        .get(args.change_id)?
        .map(|tip| {
            if tip.is_empty() {
                Ok(HgId::null_id().clone())
            } else {
                HgId::from_slice(&tip).map_err(|err| {
                    let tip = String::from_utf8_lossy(&tip).to_string();
                    RevsetLookupError::CommitHexParseError(tip, err.into()).into()
                })
            }
        })
        .transpose()
}

fn resolve_dot(args: &LookupArgs) -> Result<Option<HgId>> {
    if args.change_id != "." && !args.change_id.is_empty() {
        return Ok(None);
    }

    match args.treestate {
        Some(treestate) => match treestate.parents().next() {
            None => Ok(Some(HgId::null_id().clone())),
            Some(hgid) => Ok(Some(hgid?)),
        },
        None => Ok(None),
    }
}

fn resolve_hash_prefix(args: &LookupArgs) -> Result<Option<HgId>> {
    if !args
        .change_id
        .chars()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Ok(None);
    }
    let mut vertices = async_runtime::block_on(async {
        args.id_map
            .vertexes_by_hex_prefix(args.change_id.as_bytes(), 5)
            .await
    })?
    .into_iter();

    let vertex = if let Some(v) = vertices.next() {
        v.to_hex()
    } else {
        return Ok(None);
    };

    if let Some(vertex2) = vertices.next() {
        let mut possible_identifiers = vec![vertex, vertex2.to_hex()];
        for vertex in vertices {
            possible_identifiers.push(vertex.to_hex());
        }
        return Err(RevsetLookupError::AmbiguousIdentifier(
            args.change_id.to_owned(),
            possible_identifiers.join(", "),
        )
        .into());
    }

    Ok(Some(HgId::from_str(&vertex).map_err(|err| {
        RevsetLookupError::CommitHexParseError(vertex, err.into())
    })?))
}

fn resolve_bookmark(args: &LookupArgs) -> Result<Option<HgId>> {
    let mut local_bookmarks = metalog_bookmarks(args.metalog, "bookmarks", decode_bookmarks)?;
    if let Some(hash) = local_bookmarks.remove(args.change_id) {
        return Ok(Some(hash));
    }

    let mut remote_bookmarks = metalog_bookmarks(args.metalog, "remotenames", decode_remotenames)?;
    if let Some(hash) = remote_bookmarks.remove(args.change_id) {
        return Ok(Some(hash));
    }

    if let Some(hoist) = args.config.get("remotenames", "hoist") {
        if let Some(hash) = remote_bookmarks.remove(&format!("{}/{}", hoist, args.change_id)) {
            return Ok(Some(hash));
        }
    }

    Ok(None)
}

fn metalog_bookmarks(
    metalog: &MetaLog,
    bookmark_type: &str,
    decoder: fn(&[u8]) -> std::io::Result<BTreeMap<String, HgId>>,
) -> Result<BTreeMap<String, HgId>> {
    let raw_bookmarks = match metalog.get(bookmark_type)? {
        None => {
            return Ok(Default::default());
        }
        Some(raw_bookmarks) => raw_bookmarks.into_vec(),
    };

    Ok(decoder(raw_bookmarks.as_slice())
        .map_err(|err| RevsetLookupError::BookmarkDecodeError(bookmark_type.to_owned(), err))?)
}
