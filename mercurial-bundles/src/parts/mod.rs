// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::fmt;

use bytes::Bytes;
use failure::err_msg;
use futures::{Future, Stream};
use futures::stream::{iter_ok, once};
use futures_trace::{self, Traced};

use super::changegroup::{CgDeltaChunk, Part, Section};
use super::changegroup::packer::Cg2Packer;
use super::wirepack;
use super::wirepack::packer::WirePackPacker;

use errors::*;
use mercurial::{BlobNode, NodeHash, NULL_HASH};
use mercurial_types::{Delta, Entry, MPath, RepoPath};
use part_encode::PartEncodeBuilder;
use part_header::PartHeaderType;

pub fn listkey_part<N, S, K, V>(namespace: N, items: S) -> Result<PartEncodeBuilder>
where
    N: Into<Bytes>,
    S: Stream<Item = (K, V), Error = Error> + Send + 'static,
    K: AsRef<[u8]>,
    V: AsRef<[u8]>,
{
    let mut builder = PartEncodeBuilder::mandatory(PartHeaderType::Listkeys)?;
    builder.add_mparam("namespace", namespace)?;
    // Ideally we'd use a size_hint here, but streams don't appear to have one.
    let payload = Vec::with_capacity(256);
    let fut = items
        .fold(payload, |mut payload, (key, value)| {
            payload.extend_from_slice(key.as_ref());
            payload.push(b'\t');
            payload.extend_from_slice(value.as_ref());
            payload.push(b'\n');
            Ok::<_, Error>(payload)
        })
        .map_err(|err| Error::from(err.context(ErrorKind::ListkeyGeneration)));

    builder.set_data_future(fut);

    Ok(builder)
}

pub fn changegroup_part<S>(changelogentries: S) -> Result<PartEncodeBuilder>
where
    S: Stream<Item = BlobNode, Error = Error> + Send + 'static,
{
    let mut builder = PartEncodeBuilder::mandatory(PartHeaderType::Changegroup)?;
    builder.add_mparam("version", "02")?;

    let changelogentries = changelogentries.map(|blobnode| {
        let node = blobnode.nodeid().expect("blobnode should store data");
        let parents = blobnode.parents().get_nodes();
        let p1 = *parents.0.unwrap_or(&NULL_HASH);
        let p2 = *parents.1.unwrap_or(&NULL_HASH);
        let base = NULL_HASH;
        // Linknode is the same as node
        let linknode = node;
        let text = blobnode
            .as_blob()
            .as_inner()
            .unwrap_or(&Bytes::new())
            .clone();
        let delta = Delta::new_fulltext(text.to_vec());

        let deltachunk = CgDeltaChunk {
            node,
            p1,
            p2,
            base,
            linknode,
            delta,
        };
        Part::CgChunk(Section::Changeset, deltachunk)
    });

    let changelogentries = changelogentries
        .chain(once(Ok(Part::SectionEnd(Section::Changeset))))
        // One more SectionEnd entry is necessary because hg client excepts filelog section
        // even if it's empty. Add a fake SectionEnd part (the choice of
        // Manifest is just for convenience).
        .chain(once(Ok(Part::SectionEnd(Section::Manifest))))
        .chain(once(Ok(Part::End)));

    let cgdata = Cg2Packer::new(changelogentries);
    builder.set_data_generated(cgdata);

    Ok(builder)
}

pub fn treepack_part<S>(entries: S) -> Result<PartEncodeBuilder>
where
    S: Stream<Item = (Box<Entry + Sync>, NodeHash, Option<MPath>), Error = Error> + Send + 'static,
{
    let mut builder = PartEncodeBuilder::mandatory(PartHeaderType::B2xTreegroup2)?;
    builder.add_mparam("version", "1")?;
    builder.add_mparam("cache", "True")?;
    builder.add_mparam("category", "manifests")?;

    let buffer_size = 100; // TODO(stash): make it configurable
    let wirepack_parts = entries
        .map(|(entry, linknode, basepath)| {
            let parents = entry
                .get_parents()
                .traced_global("fetching parents", trace_args!());

            let raw_content = entry
                .get_raw_content()
                .and_then(|blob| blob.into_inner().ok_or(err_msg("bad blob content")))
                .traced_global("fetching raw content", trace_args!());

            parents
                .join(raw_content)
                .map(move |(parents, raw_content)| {
                    (entry, parents, raw_content, linknode, basepath)
                })
        })
        .buffered(buffer_size)
        .map(|(entry, parents, content, linknode, basepath)| {
            let path = match MPath::join_element_opt(basepath.as_ref(), entry.get_name()) {
                Some(path) => RepoPath::DirectoryPath(path),
                None => RepoPath::RootPath,
            };
            (entry, parents, content, linknode, path)
        })
        .map(|(entry, parents, content, linknode, path)| {
            let history_meta = wirepack::Part::HistoryMeta {
                path: path.clone(),
                entry_count: 1,
            };

            let node = NodeHash::new(entry.get_hash().into_nodehash().sha1().clone());
            let (p1, p2) = parents.get_nodes();
            let p1 = p1.map(|p| NodeHash::new(p.sha1().clone()))
                .unwrap_or(NULL_HASH);
            let p2 = p2.map(|p| NodeHash::new(p.sha1().clone()))
                .unwrap_or(NULL_HASH);

            let history = wirepack::Part::History(wirepack::HistoryEntry {
                node: node.clone(),
                p1,
                p2,
                linknode,
                // No copies/renames for trees
                copy_from: None,
            });

            let data_meta = wirepack::Part::DataMeta {
                path,
                entry_count: 1,
            };

            let data = wirepack::Part::Data(wirepack::DataEntry {
                node,
                delta_base: NULL_HASH,
                delta: Delta::new_fulltext(content.to_vec()),
            });

            iter_ok(vec![history_meta, history, data_meta, data].into_iter())
        })
        .flatten()
        .chain(once(Ok(wirepack::Part::End)));

    let packer = WirePackPacker::new(wirepack_parts, wirepack::Kind::Tree);
    builder.set_data_generated(packer);

    Ok(builder)
}

pub enum ChangegroupApplyResult {
    Success { heads_num_diff: i64 },
    Error,
}

// Mercurial source code comments are a bit contradictory:
//
// From mercurial/changegroup.py
// Return an integer summarizing the change to this repo:
// - nothing changed or no source: 0
// - more heads than before: 1+added heads (2..n)
// - fewer heads than before: -1-removed heads (-2..-n)
// - number of heads stays the same: 1
//
// From mercurial/exchange.py
// Integer version of the changegroup push result
// - None means nothing to push
// - 0 means HTTP error
// - 1 means we pushed and remote head count is unchanged *or*
//   we have outgoing changesets but refused to push
// - other values as described by addchangegroup()
//
// We are using 0 to indicate a error, 1 + heads_num_diff if the number of heads increased,
// -1 + heads_num_diff if the number of heads decreased. Note that we may change it in the future

impl fmt::Display for ChangegroupApplyResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &ChangegroupApplyResult::Success { heads_num_diff } => {
                if heads_num_diff >= 0 {
                    write!(f, "{}", 1 + heads_num_diff)
                } else {
                    write!(f, "{}", -1 + heads_num_diff)
                }
            }
            &ChangegroupApplyResult::Error => write!(f, "0"),
        }
    }
}

pub fn replychangegroup_part(
    res: ChangegroupApplyResult,
    in_reply_to: u32,
) -> Result<PartEncodeBuilder> {
    let mut builder = PartEncodeBuilder::mandatory(PartHeaderType::ReplyChangegroup)?;
    builder.add_mparam("return", format!("{}", res))?;
    builder.add_mparam("in-reply-to", format!("{}", in_reply_to))?;

    Ok(builder)
}

pub fn replypushkey_part(res: bool, in_reply_to: u32) -> Result<PartEncodeBuilder> {
    let mut builder = PartEncodeBuilder::mandatory(PartHeaderType::ReplyPushkey)?;
    if res {
        builder.add_mparam("return", "1")?;
    } else {
        builder.add_mparam("return", "0")?;
    }
    builder.add_mparam("in-reply-to", format!("{}", in_reply_to))?;

    Ok(builder)
}
