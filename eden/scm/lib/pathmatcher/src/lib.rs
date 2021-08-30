/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

mod gitignore_matcher;
mod tree_matcher;
mod utils;

use std::ops::Deref;
use std::sync::Arc;

use anyhow::Result;

use types::RepoPath;

/// Limits the set of files to be operated on.
pub trait Matcher {
    /// This method is intended for tree traversals of the file system.
    /// It allows for fast paths where whole subtrees are skipped.
    /// It should be noted that the DirectoryMatch::ShouldTraverse return value is always correct.
    /// Other values enable fast code paths only (performance).
    fn matches_directory(&self, path: &RepoPath) -> Result<DirectoryMatch>;

    /// Returns true when the file path should be kept in the file set and returns false when
    /// it has to be removed.
    fn matches_file(&self, path: &RepoPath) -> Result<bool>;
}

/// Allows for fast code paths when dealing with patterns selecting directories.
/// `Everything` means that all the files in the subtree of the given directory need to be part
/// of the returned file set.
/// `Nothing` means that no files in the subtree of the given directory will be part of the
/// returned file set. Recursive traversal can be stopped at this point.
/// `ShouldTraverse` is a value that is always valid. It does not provide additional information.
/// Subtrees should be traversed and the matches should continue to be asked.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum DirectoryMatch {
    Everything,
    Nothing,
    ShouldTraverse,
}

impl<T: Matcher + ?Sized, U: Deref<Target = T>> Matcher for U {
    fn matches_directory(&self, path: &RepoPath) -> Result<DirectoryMatch> {
        T::matches_directory(self, path)
    }

    fn matches_file(&self, path: &RepoPath) -> Result<bool> {
        T::matches_file(self, path)
    }
}

pub struct AlwaysMatcher {}

impl AlwaysMatcher {
    pub fn new() -> Self {
        AlwaysMatcher {}
    }
}

impl Matcher for AlwaysMatcher {
    fn matches_directory(&self, _path: &RepoPath) -> Result<DirectoryMatch> {
        Ok(DirectoryMatch::Everything)
    }
    fn matches_file(&self, _path: &RepoPath) -> Result<bool> {
        Ok(true)
    }
}

pub struct NeverMatcher {}

impl NeverMatcher {
    pub fn new() -> Self {
        NeverMatcher {}
    }
}

impl Matcher for NeverMatcher {
    fn matches_directory(&self, _path: &RepoPath) -> Result<DirectoryMatch> {
        Ok(DirectoryMatch::Nothing)
    }
    fn matches_file(&self, _path: &RepoPath) -> Result<bool> {
        Ok(false)
    }
}

pub struct XorMatcher<A, B> {
    a: A,
    b: B,
}

impl<A, B> XorMatcher<A, B> {
    pub fn new(a: A, b: B) -> Self {
        XorMatcher { a, b }
    }
}

impl<A: Matcher, B: Matcher> Matcher for XorMatcher<A, B> {
    fn matches_directory(&self, path: &RepoPath) -> Result<DirectoryMatch> {
        let matches_a = self.a.matches_directory(path)?;
        let matches_b = self.b.matches_directory(path)?;
        Ok(match (matches_a, matches_b) {
            (DirectoryMatch::Everything, DirectoryMatch::Everything) => DirectoryMatch::Nothing,
            (DirectoryMatch::Nothing, DirectoryMatch::Nothing) => DirectoryMatch::Nothing,
            (DirectoryMatch::Everything, DirectoryMatch::Nothing) => DirectoryMatch::Everything,
            (DirectoryMatch::Nothing, DirectoryMatch::Everything) => DirectoryMatch::Everything,
            _ => DirectoryMatch::ShouldTraverse,
        })
    }

    fn matches_file(&self, path: &RepoPath) -> Result<bool> {
        Ok(self.a.matches_file(path)? ^ self.b.matches_file(path)?)
    }
}

pub struct DifferenceMatcher<A, B> {
    include: A,
    exclude: B,
}

impl<A, B> DifferenceMatcher<A, B> {
    pub fn new(include: A, exclude: B) -> Self {
        DifferenceMatcher { include, exclude }
    }
}

impl<A: Matcher, B: Matcher> Matcher for DifferenceMatcher<A, B> {
    fn matches_directory(&self, path: &RepoPath) -> Result<DirectoryMatch> {
        let include = self.include.matches_directory(path)?;
        let exclude = self.exclude.matches_directory(path)?;

        Ok(match exclude {
            DirectoryMatch::Nothing => include,
            DirectoryMatch::Everything => DirectoryMatch::Nothing,
            DirectoryMatch::ShouldTraverse => match include {
                DirectoryMatch::Everything => DirectoryMatch::ShouldTraverse,
                _ => include,
            },
        })
    }

    fn matches_file(&self, path: &RepoPath) -> Result<bool> {
        Ok(self.include.matches_file(path)? && !self.exclude.matches_file(path)?)
    }
}

pub struct UnionMatcher {
    matchers: Vec<Arc<dyn 'static + Matcher + Send + Sync>>,
}

impl UnionMatcher {
    pub fn new(matchers: Vec<Arc<dyn 'static + Matcher + Send + Sync>>) -> Self {
        UnionMatcher { matchers }
    }
}

impl Matcher for UnionMatcher {
    fn matches_directory(&self, path: &RepoPath) -> Result<DirectoryMatch> {
        let mut current = DirectoryMatch::Nothing;
        for matcher in self.matchers.iter() {
            current = match matcher.matches_directory(path)? {
                DirectoryMatch::Nothing => current,
                DirectoryMatch::Everything => {
                    return Ok(DirectoryMatch::Everything);
                }
                DirectoryMatch::ShouldTraverse => DirectoryMatch::ShouldTraverse,
            };
        }
        Ok(current)
    }

    fn matches_file(&self, path: &RepoPath) -> Result<bool> {
        for matcher in self.matchers.iter() {
            if matcher.matches_file(path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

pub use gitignore_matcher::GitignoreMatcher;
pub use tree_matcher::TreeMatcher;
pub use utils::{expand_curly_brackets, normalize_glob, plain_to_glob};
