/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::fmt;

#[cfg(any(test, feature = "for-tests"))]
use quickcheck::Arbitrary;
use serde_derive::{Deserialize, Serialize};

/// Directory entry metadata
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectoryMetadata {
    pub fsnode_id: Option<FsnodeId>,
    pub simple_format_sha1: Option<Sha1>,
    pub simple_format_sha256: Option<Sha256>,
    pub child_files_count: Option<u64>,
    pub child_files_total_size: Option<u64>,
    pub child_dirs_count: Option<u64>,
    pub descendant_files_count: Option<u64>,
    pub descendant_files_total_size: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryMetadataRequest {
    pub with_fsnode_id: bool,
    pub with_simple_format_sha1: bool,
    pub with_simple_format_sha256: bool,
    pub with_child_files_count: bool,
    pub with_child_files_total_size: bool,
    pub with_child_dirs_count: bool,
    pub with_descendant_files_count: bool,
    pub with_descendant_files_total_size: bool,
}

/// File entry metadata
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileMetadata {
    pub revisionstore_flags: Option<u64>,
    pub content_id: Option<ContentId>,
    pub file_type: Option<FileType>,
    pub size: Option<u64>,
    pub content_sha1: Option<Sha1>,
    pub content_sha256: Option<Sha256>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMetadataRequest {
    pub with_revisionstore_flags: bool,
    pub with_content_id: bool,
    pub with_file_type: bool,
    pub with_size: bool,
    pub with_content_sha1: bool,
    pub with_content_sha256: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sha1(pub [u8; 20]);

impl fmt::Display for Sha1 {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "Sha1(\"")?;
        for d in &self.0 {
            write!(fmt, "{:02x}", d)?;
        }
        write!(fmt, "\")")
    }
}

impl fmt::Debug for Sha1 {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "Sha1(\"")?;
        for d in &self.0 {
            write!(fmt, "{:02x}", d)?;
        }
        write!(fmt, "\")")
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sha256(pub [u8; 32]);

impl fmt::Display for Sha256 {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "Sha256(\"")?;
        for d in &self.0 {
            write!(fmt, "{:02x}", d)?;
        }
        write!(fmt, "\")")
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "Sha256(\"")?;
        for d in &self.0 {
            write!(fmt, "{:02x}", d)?;
        }
        write!(fmt, "\")")
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentId(pub [u8; 32]);

impl fmt::Display for ContentId {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "ContentId(\"")?;
        for d in &self.0 {
            write!(fmt, "{:02x}", d)?;
        }
        write!(fmt, "\")")
    }
}

impl fmt::Debug for ContentId {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "ContentId(\"")?;
        for d in &self.0 {
            write!(fmt, "{:02x}", d)?;
        }
        write!(fmt, "\")")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileType {
    Regular,
    Executable,
    Symlink,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsnodeId(pub [u8; 32]);

impl fmt::Display for FsnodeId {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "FsnodeId(\"")?;
        for d in &self.0 {
            write!(fmt, "{:02x}", d)?;
        }
        write!(fmt, "\")")
    }
}

impl fmt::Debug for FsnodeId {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "FsnodeId(\"")?;
        for d in &self.0 {
            write!(fmt, "{:02x}", d)?;
        }
        write!(fmt, "\")")
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

        let variant = u32::arbitrary(g) % 3;
        match variant {
            0 => Regular,
            1 => Executable,
            2 => Symlink,
            _ => unreachable!(),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for FsnodeId {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        let mut v = Self::default();
        for b in v.0.iter_mut() {
            *b = u8::arbitrary(g);
        }
        v
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for ContentId {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        let mut v = Self::default();
        for b in v.0.iter_mut() {
            *b = u8::arbitrary(g);
        }
        v
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for Sha1 {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        let mut v = Self::default();
        for b in v.0.iter_mut() {
            *b = u8::arbitrary(g);
        }
        v
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for Sha256 {
    fn arbitrary(g: &mut quickcheck::Gen) -> Self {
        let mut v = Self::default();
        for b in v.0.iter_mut() {
            *b = u8::arbitrary(g);
        }
        v
    }
}
