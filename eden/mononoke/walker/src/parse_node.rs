/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::graph::{AliasType, Node, NodeType, WrappedPath};

use anyhow::{format_err, Error};
use bookmarks::BookmarkName;
use filestore::Alias;
use mercurial_types::{HgChangesetId, HgFileNodeId, HgManifestId};
use mononoke_types::{
    hash::{GitSha1, Sha1, Sha256},
    ChangesetId, ContentId, FsnodeId, MPath,
};
use std::iter::FromIterator;
use std::str::FromStr;
use strum::IntoEnumIterator;

const NODE_SEP: &str = ":";

fn check_and_build_mpath(node_type: NodeType, parts: &[&str]) -> Result<Option<MPath>, Error> {
    if parts.len() < 2 {
        return Err(format_err!(
            "parse_node requires a path and key for {}",
            node_type
        ));
    }
    let mpath = match parts[1..].join(NODE_SEP).as_str() {
        "/" => None,
        p => Some(MPath::new(p)?),
    };
    Ok(mpath)
}

pub fn parse_node(s: &str) -> Result<Node, Error> {
    let parts: Vec<_> = s.split(NODE_SEP).collect();
    if parts.len() < 1 {
        return Err(format_err!("parse_node requires at least NodeType"));
    }
    let node_type = NodeType::from_str(parts[0])?;
    match (node_type, parts.len()) {
        (NodeType::Root, 1) | (NodeType::PublishedBookmarks, 1) => {}
        (NodeType::Root, _) | (NodeType::PublishedBookmarks, _) => {
            return Err(format_err!(
                "parse_node expects {} not to be followed by any parts",
                node_type
            ));
        }
        (_, l) if l < 2 => {
            return Err(format_err!(
                "parse_node for {} requires at least NodeType:node_key",
                node_type
            ));
        }
        _ => {}
    }

    let parts = &parts[1..];
    let node = match node_type {
        NodeType::Root => Node::Root(()),
        // Bonsai
        NodeType::Bookmark => Node::Bookmark(BookmarkName::new(parts.join(NODE_SEP))?),
        NodeType::BonsaiChangeset => {
            Node::BonsaiChangeset(ChangesetId::from_str(&parts.join(NODE_SEP))?)
        }
        NodeType::BonsaiHgMapping => {
            Node::BonsaiHgMapping(ChangesetId::from_str(&parts.join(NODE_SEP))?)
        }
        NodeType::BonsaiPhaseMapping => {
            Node::BonsaiPhaseMapping(ChangesetId::from_str(&parts.join(NODE_SEP))?)
        }
        NodeType::PublishedBookmarks => Node::PublishedBookmarks(()),
        // Hg
        NodeType::HgBonsaiMapping => {
            Node::HgBonsaiMapping(HgChangesetId::from_str(&parts.join(NODE_SEP))?)
        }
        NodeType::HgChangeset => Node::HgChangeset(HgChangesetId::from_str(&parts.join(NODE_SEP))?),
        NodeType::HgManifest => {
            let mpath = check_and_build_mpath(node_type, parts)?;
            let id = HgManifestId::from_str(parts[0])?;
            Node::HgManifest((WrappedPath::from(mpath), id))
        }
        NodeType::HgFileEnvelope => {
            Node::HgFileEnvelope(HgFileNodeId::from_str(&parts.join(NODE_SEP))?)
        }
        NodeType::HgFileNode => {
            let mpath = check_and_build_mpath(node_type, parts)?;
            let id = HgFileNodeId::from_str(parts[0])?;
            Node::HgFileNode((WrappedPath::from(mpath), id))
        }
        // Content
        NodeType::FileContent => Node::FileContent(ContentId::from_str(&parts.join(NODE_SEP))?),
        NodeType::FileContentMetadata => {
            Node::FileContentMetadata(ContentId::from_str(&parts.join(NODE_SEP))?)
        }
        NodeType::AliasContentMapping => {
            if parts.len() < 2 {
                return Err(format_err!(
                    "parse_node requires an alias type from {:?} and key for {}",
                    Vec::from_iter(AliasType::iter()),
                    node_type
                ));
            }
            let alias_type = AliasType::from_str(parts[0])?;
            let id = &parts[1..].join(NODE_SEP);
            let alias = match alias_type {
                AliasType::GitSha1 => Alias::GitSha1(GitSha1::from_str(id)?),
                AliasType::Sha1 => Alias::Sha1(Sha1::from_str(id)?),
                AliasType::Sha256 => Alias::Sha256(Sha256::from_str(id)?),
            };
            Node::AliasContentMapping(alias)
        }
        // Derived data
        NodeType::BonsaiFsnodeMapping => {
            Node::BonsaiFsnodeMapping(ChangesetId::from_str(&parts.join(NODE_SEP))?)
        }
        NodeType::ChangesetInfo => {
            Node::ChangesetInfo(ChangesetId::from_str(&parts.join(NODE_SEP))?)
        }
        NodeType::Fsnode => {
            let mpath = check_and_build_mpath(node_type, parts)?;
            let id = FsnodeId::from_str(parts[0])?;
            Node::Fsnode((WrappedPath::from(mpath), id))
        }
    };
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_BLAKE2: &str = "b847b8838bfe3ae13ea6f8ce2e341c51193587b8392494f6dbab7224b3b116bf";
    const SAMPLE_SHA1: &str = "e797dcabdd6d16ec4ae614165178b60d7054305b";
    const SAMPLE_SHA256: &str = "332ff483aaf1bbc241314576b399f81675a6f81aba205bd3b80b05a4ffda44d4";
    const SAMPLE_PATH: &str = "/foo/bar/baz";

    fn test_node_type(node_type: &NodeType) -> Result<(), Error> {
        let v = match node_type {
            NodeType::Root => {
                assert_eq!(Node::Root(()), parse_node("Root")?);
                assert_eq!(
                    "Err(parse_node expects Root not to be followed by any parts)",
                    format!("{:?}", parse_node("Root:garbage"))
                );
            }
            NodeType::Bookmark => assert_eq!(
                Node::Bookmark(BookmarkName::new("foo")?),
                parse_node(&format!("Bookmark{}foo", NODE_SEP))?
            ),
            NodeType::BonsaiChangeset => assert_eq!(
                node_type,
                &parse_node(&format!("BonsaiChangeset{}{}", NODE_SEP, SAMPLE_BLAKE2))?.get_type()
            ),
            NodeType::BonsaiHgMapping => assert_eq!(
                node_type,
                &parse_node(&format!("BonsaiHgMapping{}{}", NODE_SEP, SAMPLE_BLAKE2))?.get_type()
            ),
            NodeType::BonsaiPhaseMapping => assert_eq!(
                node_type,
                &parse_node(&format!("BonsaiPhaseMapping{}{}", NODE_SEP, SAMPLE_BLAKE2))?
                    .get_type()
            ),
            NodeType::PublishedBookmarks => {
                assert_eq!(
                    Node::PublishedBookmarks(()),
                    parse_node("PublishedBookmarks")?
                );
                assert_eq!(
                    "Err(parse_node expects PublishedBookmarks not to be followed by any parts)",
                    format!("{:?}", parse_node("PublishedBookmarks:garbage"))
                );
            }
            // Hg
            NodeType::HgBonsaiMapping => assert_eq!(
                node_type,
                &parse_node(&format!("HgBonsaiMapping{}{}", NODE_SEP, SAMPLE_SHA1))?.get_type()
            ),
            NodeType::HgChangeset => assert_eq!(
                node_type,
                &parse_node(&format!("HgChangeset{}{}", NODE_SEP, SAMPLE_SHA1))?.get_type()
            ),
            NodeType::HgManifest => assert_eq!(
                node_type,
                &parse_node(&format!(
                    "HgManifest{}{}{}{}",
                    NODE_SEP, SAMPLE_SHA1, NODE_SEP, SAMPLE_PATH
                ))?
                .get_type()
            ),
            NodeType::HgFileEnvelope => assert_eq!(
                node_type,
                &parse_node(&format!("HgFileEnvelope{}{}", NODE_SEP, SAMPLE_SHA1))?.get_type()
            ),
            NodeType::HgFileNode => assert_eq!(
                node_type,
                &parse_node(&format!(
                    "HgFileNode{}{}{}{}",
                    NODE_SEP, SAMPLE_SHA1, NODE_SEP, SAMPLE_PATH
                ))?
                .get_type()
            ),
            // Content
            NodeType::FileContent => assert_eq!(
                node_type,
                &parse_node(&format!("FileContent{}{}", NODE_SEP, SAMPLE_BLAKE2))?.get_type()
            ),
            NodeType::FileContentMetadata => assert_eq!(
                node_type,
                &parse_node(&format!("FileContentMetadata{}{}", NODE_SEP, SAMPLE_BLAKE2))?
                    .get_type()
            ),
            NodeType::AliasContentMapping => {
                assert_eq!(
                    node_type,
                    &parse_node(&format!(
                        "AliasContentMapping{}{}{}{}",
                        NODE_SEP, "Sha1", NODE_SEP, SAMPLE_SHA1
                    ))?
                    .get_type()
                );
                assert_eq!(
                    node_type,
                    &parse_node(&format!(
                        "AliasContentMapping{}{}{}{}",
                        NODE_SEP, "Sha256", NODE_SEP, SAMPLE_SHA256
                    ))?
                    .get_type()
                );
            }
            NodeType::BonsaiFsnodeMapping => {
                assert_eq!(
                    node_type,
                    &parse_node(&format!("BonsaiFsnodeMapping{}{}", NODE_SEP, SAMPLE_BLAKE2))?
                        .get_type()
                );
            }
            NodeType::ChangesetInfo => {
                assert_eq!(
                    node_type,
                    &parse_node(&format!("ChangesetInfo{}{}", NODE_SEP, SAMPLE_BLAKE2))?.get_type()
                );
            }
            NodeType::Fsnode => {
                assert_eq!(
                    node_type,
                    &parse_node(&format!(
                        "Fsnode{}{}{}{}",
                        NODE_SEP, SAMPLE_BLAKE2, NODE_SEP, SAMPLE_PATH
                    ))?
                    .get_type()
                );
            }
        };
        Ok(v)
    }

    #[test]
    fn parse_all_node_types() -> Result<(), Error> {
        for t in NodeType::iter() {
            test_node_type(&t)?;
        }
        Ok(())
    }
}
