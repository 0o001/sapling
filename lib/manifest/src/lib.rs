// Copyright 2019 Facebook, Inc.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! manifest - The contents of the repository at a specific commit.
//!
//! The history of the repository is recorded in the commit graph. Each commit has a manifest
//! associated with it. The manifest specifies the revision for all the files available in the
//! repository. The file path and file revision are then used to retrieve the contents of the
//! file thus achieving the reconstruction of the entire repository state.

use failure::Fallible;

use types::{Node, RepoPath, RepoPathBuf};

/// Manifest describes a mapping between file path ([`String`]) and file metadata ([`FileMetadata`]).
/// Fundamentally it is just a Map<file_path, file_metadata>.
///
/// It can be assumed that Manifest interacts with an underlying store for persistance. These
/// interactions may fail due to a variety of reasons. Such failures will be propagated up as Error
/// return statuses.
///
/// Another common failure is passing in a path that the manifest has labeled as a directory. File
/// paths composed of directory names and file names. Querying for paths that the Manifest has
/// determined previously to be directories will result in Errors.
pub trait Manifest {
    /// Retrieve the FileMetadata that is associated with a path.
    /// Paths that were not set will return None.
    fn get(&self, file_path: &RepoPath) -> Fallible<Option<&FileMetadata>>;

    /// Associates a file path with specific file metadata.
    /// A call with a file path that already exists results in an override or the old metadata.
    fn insert(&mut self, file_path: RepoPathBuf, file_metadata: FileMetadata) -> Fallible<()>;

    /// Removes a file from the manifest (equivalent to removing it from the repository).
    /// A call with a file path that does not exist in the manifest is a no-op.
    fn remove(&mut self, file_path: &RepoPath) -> Fallible<()>;

    /// Persists the manifest so that it can be retrieved at a later time. Returns a note
    /// representing the identifier for saved manifest.
    fn flush(&mut self) -> Fallible<Node>;
}

/// The contents of the Manifest for a file.
/// * node: used to determine the revision of the file in the repository.
/// * file_type: determines the type of the file.
#[derive(Clone, Copy, Debug, Default, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct FileMetadata {
    pub node: Node,
    pub file_type: FileType,
}

/// The types of files that are supported.
///
/// The debate here is whether to use Regular { executable: bool } or an Executable variant.
/// Technically speaking executable files are regular files. There is no big difference in terms
/// of the mechanics between the two approaches. The approach using an Executable is more readable
/// so that is what we have now.
#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub enum FileType {
    /// Regular files.
    Regular,
    /// Executable files. Like Regular files but with the executable flag set.
    Executable,
    /// Symlinks. Their targets are not limited to repository paths. They can point anywhere.
    Symlink,
}

impl Default for FileType {
    fn default() -> Self {
        FileType::Regular
    }
}

impl FileMetadata {
    pub fn new(node: Node, file_type: FileType) -> Self {
        Self { node, file_type }
    }

    /// Creates `FileMetadata` with file_type set to `FileType::Regular`.
    pub fn regular(node: Node) -> Self {
        Self::new(node, FileType::Regular)
    }

    /// Creates `FileMetadata` with file_type set to `FileType::Executable`.
    pub fn executable(node: Node) -> Self {
        Self::new(node, FileType::Executable)
    }

    /// Creates `FileMetadata` with file_type set to `FileType::Symlink`.
    pub fn symlink(node: Node) -> Self {
        Self::new(node, FileType::Symlink)
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl quickcheck::Arbitrary for FileType {
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        g.choose(&[FileType::Regular, FileType::Executable, FileType::Symlink])
            .unwrap()
            .to_owned()
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl quickcheck::Arbitrary for FileMetadata {
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        let node = Node::arbitrary(g);
        let file_type = FileType::arbitrary(g);
        FileMetadata::new(node, file_type)
    }
}

mod tree;
pub use crate::tree::Tree;
