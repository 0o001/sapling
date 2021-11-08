/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::fmt;
use std::str::FromStr;

#[cfg(any(test, feature = "for-tests"))]
use quickcheck::Arbitrary;
use serde_derive::Deserialize;
use serde_derive::Serialize;
use type_macros::auto_wire;

use crate::ServerError;

/// Directory entry metadata
#[auto_wire]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectoryMetadata {
    #[id(0)]
    pub fsnode_id: Option<FsnodeId>,
    #[id(1)]
    pub simple_format_sha1: Option<Sha1>,
    #[id(2)]
    pub simple_format_sha256: Option<Sha256>,
    #[id(3)]
    pub child_files_count: Option<u64>,
    #[id(4)]
    pub child_files_total_size: Option<u64>,
    #[id(5)]
    pub child_dirs_count: Option<u64>,
    #[id(6)]
    pub descendant_files_count: Option<u64>,
    #[id(7)]
    pub descendant_files_total_size: Option<u64>,
}

#[auto_wire]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryMetadataRequest {
    #[id(0)]
    pub with_fsnode_id: bool,
    #[id(1)]
    pub with_simple_format_sha1: bool,
    #[id(2)]
    pub with_simple_format_sha256: bool,
    #[id(3)]
    pub with_child_files_count: bool,
    #[id(4)]
    pub with_child_files_total_size: bool,
    #[id(5)]
    pub with_child_dirs_count: bool,
    #[id(6)]
    pub with_descendant_files_count: bool,
    #[id(7)]
    pub with_descendant_files_total_size: bool,
}

/// File entry metadata
#[auto_wire]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileMetadata {
    #[id(0)]
    pub revisionstore_flags: Option<u64>,
    #[id(1)]
    pub content_id: Option<ContentId>,
    #[id(2)]
    pub file_type: Option<FileType>,
    #[id(3)]
    pub size: Option<u64>,
    #[id(4)]
    pub content_sha1: Option<Sha1>,
    #[id(5)]
    pub content_sha256: Option<Sha256>,
}

#[auto_wire]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMetadataRequest {
    #[id(0)]
    pub with_revisionstore_flags: bool,
    #[id(1)]
    pub with_content_id: bool,
    #[id(2)]
    pub with_file_type: bool,
    #[id(3)]
    pub with_size: bool,
    #[id(4)]
    pub with_content_sha1: bool,
    #[id(5)]
    pub with_content_sha256: bool,
}

sized_hash!(Sha1, 20);
sized_hash!(Sha256, 32);
blake2_hash!(ContentId);
blake2_hash!(FsnodeId);

#[auto_wire]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileType {
    #[id(1)]
    Regular,
    #[id(2)]
    Executable,
    #[id(3)]
    Symlink,
}

impl Default for FileType {
    fn default() -> Self {
        Self::Regular
    }
}

#[auto_wire]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum AnyFileContentId {
    #[id(1)]
    ContentId(ContentId),
    #[id(2)]
    Sha1(Sha1),
    #[id(3)]
    Sha256(Sha256),
}

impl Default for AnyFileContentId {
    fn default() -> Self {
        AnyFileContentId::ContentId(ContentId::default())
    }
}

impl FromStr for AnyFileContentId {
    type Err = ServerError;

    fn from_str(s: &str) -> Result<AnyFileContentId, Self::Err> {
        let v: Vec<&str> = s.split('/').collect();
        if v.len() != 2 {
            return Err(Self::Err::generic(
                "AnyFileContentId parsing failure: format is 'idtype/id'",
            ));
        }
        let idtype = v[0];
        let id = v[1];
        let any_file_content_id = match idtype {
            "content_id" => AnyFileContentId::ContentId(ContentId::from_str(id)?),
            "sha1" => AnyFileContentId::Sha1(Sha1::from_str(id)?),
            "sha256" => AnyFileContentId::Sha256(Sha256::from_str(id)?),
            _ => {
                return Err(Self::Err::generic(
                    "AnyFileContentId parsing failure: supported id types are: 'content_id', 'sha1' and 'sha256'",
                ));
            }
        };
        Ok(any_file_content_id)
    }
}

impl fmt::Display for AnyFileContentId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            AnyFileContentId::ContentId(id) => write!(f, "{}", id),
            AnyFileContentId::Sha1(id) => write!(f, "{}", id),
            AnyFileContentId::Sha256(id) => write!(f, "{}", id),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for DirectoryMetadata {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        Self {
            fsnode_id: Arbitrary::arbitrary(g),
            simple_format_sha1: Arbitrary::arbitrary(g),
            simple_format_sha256: Arbitrary::arbitrary(g),
            child_files_count: Arbitrary::arbitrary(g),
            child_files_total_size: Arbitrary::arbitrary(g),
            child_dirs_count: Arbitrary::arbitrary(g),
            descendant_files_count: Arbitrary::arbitrary(g),
            descendant_files_total_size: Arbitrary::arbitrary(g),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for DirectoryMetadataRequest {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        Self {
            with_fsnode_id: Arbitrary::arbitrary(g),
            with_simple_format_sha1: Arbitrary::arbitrary(g),
            with_simple_format_sha256: Arbitrary::arbitrary(g),
            with_child_files_count: Arbitrary::arbitrary(g),
            with_child_files_total_size: Arbitrary::arbitrary(g),
            with_child_dirs_count: Arbitrary::arbitrary(g),
            with_descendant_files_count: Arbitrary::arbitrary(g),
            with_descendant_files_total_size: Arbitrary::arbitrary(g),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for FileMetadata {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        Self {
            revisionstore_flags: Arbitrary::arbitrary(g),
            content_id: Arbitrary::arbitrary(g),
            file_type: Arbitrary::arbitrary(g),
            size: Arbitrary::arbitrary(g),
            content_sha1: Arbitrary::arbitrary(g),
            content_sha256: Arbitrary::arbitrary(g),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for FileMetadataRequest {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        Self {
            with_revisionstore_flags: Arbitrary::arbitrary(g),
            with_content_id: Arbitrary::arbitrary(g),
            with_file_type: Arbitrary::arbitrary(g),
            with_size: Arbitrary::arbitrary(g),
            with_content_sha1: Arbitrary::arbitrary(g),
            with_content_sha256: Arbitrary::arbitrary(g),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for FileType {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        use FileType::*;

        let variant = g.choose(&[0, 1, 2]).unwrap();
        match variant {
            0 => Regular,
            1 => Executable,
            2 => Symlink,
            _ => unreachable!(),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for AnyFileContentId {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        use AnyFileContentId::*;

        let variant = g.choose(&[0, 1, 2]).unwrap();
        match variant {
            0 => ContentId(Arbitrary::arbitrary(g)),
            1 => Sha1(Arbitrary::arbitrary(g)),
            2 => Sha256(Arbitrary::arbitrary(g)),
            _ => unreachable!(),
        }
    }
}
