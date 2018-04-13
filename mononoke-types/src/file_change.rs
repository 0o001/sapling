// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use quickcheck::{empty_shrinker, single_shrinker, Arbitrary, Gen};

use errors::*;
use path::MPath;
use thrift;
use typed_hash::{ChangesetId, ContentId};

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FileChange {
    content_id: ContentId,
    file_type: FileType,
    size: u64,
    copy_from: Option<(MPath, ChangesetId)>,
}

impl FileChange {
    pub fn new(
        content_id: ContentId,
        file_type: FileType,
        size: u64,
        copy_from: Option<(MPath, ChangesetId)>,
    ) -> Self {
        // XXX maybe convert this to a builder
        Self {
            content_id,
            file_type,
            size,
            copy_from,
        }
    }

    pub(crate) fn from_thrift(fc: thrift::FileChange, mpath: &MPath) -> Result<Self> {
        let catch_block = || {
            Ok(Self {
                content_id: ContentId::from_thrift(fc.content_id)?,
                file_type: FileType::from_thrift(fc.file_type)?,
                size: fc.size as u64,
                copy_from: match fc.copy_from {
                    Some(copy_info) => Some((
                        MPath::from_thrift(copy_info.file)?,
                        ChangesetId::from_thrift(copy_info.cs_id)?,
                    )),
                    None => None,
                },
            })
        };

        Ok(catch_block().with_context(|_: &Error| {
            ErrorKind::InvalidThrift(
                "FileChange".into(),
                format!("Invalid changed entry for path {}", mpath),
            )
        })?)
    }

    pub fn content_id(&self) -> &ContentId {
        &self.content_id
    }

    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn copy_from(&self) -> Option<&(MPath, ChangesetId)> {
        self.copy_from.as_ref()
    }

    pub fn into_thrift(self) -> thrift::FileChange {
        thrift::FileChange {
            content_id: self.content_id.into_thrift(),
            file_type: self.file_type.into_thrift(),
            size: self.size as i64,
            copy_from: self.copy_from.map(|(file, cs_id)| thrift::CopyInfo {
                file: file.into_thrift(),
                cs_id: cs_id.into_thrift(),
            }),
        }
    }
}

impl Arbitrary for FileChange {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        let copy_from = if g.gen_weighted_bool(5) {
            Some((MPath::arbitrary(g), ChangesetId::arbitrary(g)))
        } else {
            None
        };
        FileChange {
            content_id: ContentId::arbitrary(g),
            file_type: FileType::arbitrary(g),
            size: u64::arbitrary(g),
            copy_from,
        }
    }

    fn shrink(&self) -> Box<Iterator<Item = Self>> {
        // The only thing that can be reduced here is copy_from.
        if self.copy_from.is_some() {
            single_shrinker(FileChange {
                content_id: self.content_id,
                file_type: self.file_type,
                size: self.size,
                copy_from: None,
            })
        } else {
            empty_shrinker()
        }
    }
}

/// Type of a file.
///
/// Regular and Executable are identical - they both represent files containing arbitrary content.
/// The only difference is that the Executables are created with executable permission when
/// checked out.
///
/// Symlink is also the same as Regular, but the content of the file is interpolated into a path
/// being traversed during lookup.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize)]
pub enum FileType {
    Regular,
    Executable,
    Symlink,
}

impl FileType {
    pub(crate) fn from_thrift(ft: thrift::FileType) -> Result<Self> {
        let file_type = match ft {
            thrift::FileType::Regular => FileType::Regular,
            thrift::FileType::Executable => FileType::Executable,
            thrift::FileType::Symlink => FileType::Symlink,
            thrift::FileType(x) => bail_err!(ErrorKind::InvalidThrift(
                "FileType".into(),
                format!("unknown file type '{}'", x)
            )),
        };
        Ok(file_type)
    }

    pub(crate) fn into_thrift(self) -> thrift::FileType {
        match self {
            FileType::Regular => thrift::FileType::Regular,
            FileType::Executable => thrift::FileType::Executable,
            FileType::Symlink => thrift::FileType::Symlink,
        }
    }
}

impl Arbitrary for FileType {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        match g.gen_range(0, 10) {
            0 => FileType::Executable,
            1 => FileType::Symlink,
            _ => FileType::Regular,
        }
    }

    fn shrink(&self) -> Box<Iterator<Item = Self>> {
        empty_shrinker()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    quickcheck! {
        fn filetype_thrift_roundtrip(ft: FileType) -> bool {
            let thrift_ft = ft.into_thrift();
            let ft2 = FileType::from_thrift(thrift_ft)
                .expect("thrift roundtrip should always be valid");
            ft == ft2
        }

        fn filechange_thrift_roundtrip(fc: FileChange) -> bool {
            let thrift_fc = fc.clone().into_thrift();
            let fc2 = FileChange::from_thrift(thrift_fc, &MPath::new("foo").unwrap())
                .expect("thrift roundtrip should always be valid");
            fc == fc2
        }
    }

    #[test]
    fn bad_filetype_thrift() {
        let thrift_ft = thrift::FileType(42);
        FileType::from_thrift(thrift_ft).expect_err("unexpected OK - unknown file type");
    }

    #[test]
    fn bad_filechange_thrift() {
        let thrift_fc = thrift::FileChange {
            content_id: thrift::ContentId(thrift::Blake2(vec![0; 16])),
            file_type: thrift::FileType::Regular,
            size: 0,
            copy_from: None,
        };
        FileChange::from_thrift(thrift_fc, &MPath::new("foo").unwrap())
            .expect_err("unexpected OK - bad content ID");
    }
}
