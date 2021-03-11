/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::{BTreeMap, BTreeSet};
use std::iter::FromIterator;

use blobrepo::BlobRepo;
use bytes::Bytes;
use chrono::{DateTime, FixedOffset};
use context::CoreContext;
use filestore::{FetchKey, StoreRequest};
use futures::{
    future::try_join3,
    stream::{self, FuturesOrdered, FuturesUnordered, Stream, TryStreamExt},
};
use futures_stats::TimedFutureExt;
use manifest::PathTree;
use mononoke_types::{
    BonsaiChangesetMut, ChangesetId, DateTime as MononokeDateTime, FileChange, MPath,
};
use sorted_vector_map::SortedVectorMap;

use crate::changeset::ChangesetContext;
use crate::errors::MononokeError;
use crate::file::{FileId, FileType};
use crate::path::MononokePath;
use crate::repo_write::RepoWriteContext;
use crate::specifiers::ChangesetSpecifier;

#[derive(Clone)]
pub struct CreateCopyInfo {
    path: MononokePath,
    parent_index: usize,
}

impl CreateCopyInfo {
    pub fn new(path: MononokePath, parent_index: usize) -> Self {
        CreateCopyInfo { path, parent_index }
    }

    async fn resolve(
        self,
        parents: &Vec<ChangesetContext>,
    ) -> Result<(MPath, ChangesetId), MononokeError> {
        let parent_ctx = parents.get(self.parent_index).ok_or_else(|| {
            MononokeError::InvalidRequest(format!(
                "Parent index '{}' out of range for commit with {} parent(s)",
                self.parent_index,
                parents.len()
            ))
        })?;
        if !parent_ctx.path(self.path.clone())?.is_file().await? {
            return Err(MononokeError::InvalidRequest(String::from(
                "Copy-from path must reference a file",
            )));
        }
        let mpath = self.path.into_mpath().ok_or_else(|| {
            MononokeError::InvalidRequest(String::from("Copy-from path cannot be the root"))
        })?;
        Ok((mpath, parent_ctx.id()))
    }
}

/// Description of a change to make to a file.
#[derive(Clone)]
pub enum CreateChange {
    /// The file is created or modified to contain new data.
    NewContent(Bytes, FileType, Option<CreateCopyInfo>),

    /// The file is created or modified to contain the same contents as an
    /// existing file
    ExistingContent(FileId, FileType, Option<CreateCopyInfo>),

    /// The file is deleted
    Delete,
}

// Enum for recording whether a path is not changed, changed or deleted.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum CreateChangeType {
    None,
    Change,
    Delete,
}

impl Default for CreateChangeType {
    fn default() -> Self {
        CreateChangeType::None
    }
}

impl CreateChange {
    pub async fn resolve(
        self,
        ctx: CoreContext,
        repo: &BlobRepo,
        parents: &Vec<ChangesetContext>,
    ) -> Result<Option<FileChange>, MononokeError> {
        match self {
            CreateChange::NewContent(bytes, file_type, copy_info) => {
                let meta = filestore::store(
                    repo.blobstore(),
                    repo.filestore_config(),
                    &ctx,
                    &StoreRequest::new(bytes.len() as u64),
                    stream::once(async move { Ok(bytes) }),
                )
                .await?;
                let copy_info = match copy_info {
                    Some(copy_info) => Some(copy_info.resolve(parents).await?),
                    None => None,
                };
                Ok(Some(FileChange::new(
                    meta.content_id,
                    file_type,
                    meta.total_size,
                    copy_info,
                )))
            }
            CreateChange::ExistingContent(file_id, file_type, copy_info) => {
                let meta =
                    filestore::get_metadata(repo.blobstore(), &ctx, &FetchKey::Canonical(file_id))
                        .await?
                        .ok_or_else(|| {
                            MononokeError::InvalidRequest(format!(
                                "File id '{}' is not available in this repo",
                                file_id
                            ))
                        })?;
                let copy_info = match copy_info {
                    Some(copy_info) => Some(copy_info.resolve(parents).await?),
                    None => None,
                };
                Ok(Some(FileChange::new(
                    meta.content_id,
                    file_type,
                    meta.total_size,
                    copy_info,
                )))
            }
            CreateChange::Delete => Ok(None),
        }
    }

    fn change_type(&self) -> CreateChangeType {
        match self {
            CreateChange::Delete => CreateChangeType::Delete,
            _ => CreateChangeType::Change,
        }
    }
}

/// Verify that all deleted files existed in at least one of the parents.
async fn verify_deleted_files_existed_in_a_parent(
    parent_ctxs: &[ChangesetContext],
    deleted_files: BTreeSet<MononokePath>,
) -> Result<(), MononokeError> {
    async fn get_matching_files<'a>(
        parent_ctx: &'a ChangesetContext,
        files: &'a BTreeSet<MononokePath>,
    ) -> Result<impl Stream<Item = Result<MononokePath, MononokeError>> + 'a, MononokeError> {
        Ok(parent_ctx
            .paths(files.iter().cloned())
            .await?
            .try_filter_map(|changeset_path| async move {
                if changeset_path.is_file().await? {
                    Ok(Some(changeset_path.path().clone()))
                } else {
                    Ok(None)
                }
            }))
    }

    // Filter the deleted files to those that existed in a parent.
    let parent_files: BTreeSet<_> = parent_ctxs
        .iter()
        .map(|parent_ctx| get_matching_files(parent_ctx, &deleted_files))
        .collect::<FuturesUnordered<_>>()
        .try_flatten()
        .try_collect()
        .await?;

    // Quickly check if all deleted files existed by comparing set lengths.
    if deleted_files.len() == parent_files.len() {
        Ok(())
    } else {
        // At least one deleted file didn't exist. Find out which ones to
        // give a good error message.
        let non_existent_path = deleted_files
            .difference(&parent_files)
            .next()
            .expect("at least one file did not exist");
        let path_count = deleted_files.len().saturating_sub(parent_files.len());
        if path_count == 1 {
            Err(MononokeError::InvalidRequest(format!(
                "Deleted file '{}' does not exist in any parent",
                non_existent_path
            )))
        } else {
            Err(MononokeError::InvalidRequest(format!(
                "{} deleted files ('{}', ...) do not exist in any parent",
                path_count, non_existent_path
            )))
        }
    }
}

/// Returns `true` if any prefix of the path has a change.  Use for
/// detecting when a directory is replaced by a file.
fn is_prefix_changed(path: &MononokePath, paths: &PathTree<CreateChangeType>) -> bool {
    path.prefixes()
        .any(|prefix| paths.get(prefix.as_mpath()) == Some(&CreateChangeType::Change))
}

/// Verify that any files in `prefix_paths` that exist in `parent_ctx` have
/// been marked as deleted in `path_changes`.
async fn verify_prefix_files_deleted(
    parent_ctx: &ChangesetContext,
    prefix_paths: &BTreeSet<MononokePath>,
    path_changes: &PathTree<CreateChangeType>,
) -> Result<(), MononokeError> {
    parent_ctx
        .paths(prefix_paths.iter().cloned())
        .await?
        .try_for_each(|prefix_path| async move {
            if prefix_path.is_file().await?
                && path_changes.get(prefix_path.path().as_mpath())
                    != Some(&CreateChangeType::Delete)
            {
                Err(MononokeError::InvalidRequest(format!(
                    "Creating files inside '{}' requires deleting the file at that path",
                    prefix_path.path()
                )))
            } else {
                Ok(())
            }
        })
        .await
}

impl RepoWriteContext {
    /// Create a new changeset in the repository.
    ///
    /// The new changeset is created with the given metadata by unioning the
    /// contents of all parent changesets and then applying the provided
    /// changes on top.
    ///
    /// Note that:
    ///   - The changes must be internally consistent (there must be no path
    ///     conflicts between changed files).
    ///   - If a file in any parent changeset is being replaced by a directory
    ///     then that file must be deleted in the set of changes.
    ///   - If a directory in any parent changeset is being replaced by a file,
    ///     then the contents of the parent directory do not need to be deleted.
    ///     If deletions for the contents of the directory are included they will
    ///     be checked for correctness (the files must exist), but they will
    ///     otherwise be ignored.
    ///   - Any merge conflicts introduced by merging the parent changesets
    ///     must be resolved by a corresponding change in the set of changes.
    ///
    /// Currenly only a single parent is supported, and root changesets (changesets
    /// with no parents) cannot be created.
    pub async fn create_changeset(
        &self,
        parents: Vec<ChangesetId>,
        author: String,
        author_date: DateTime<FixedOffset>,
        committer: Option<String>,
        committer_date: Option<DateTime<FixedOffset>>,
        message: String,
        extra: BTreeMap<String, Vec<u8>>,
        changes: BTreeMap<MononokePath, CreateChange>,
    ) -> Result<ChangesetContext, MononokeError> {
        self.check_method_permitted("create_changeset")?;

        // Merge rules are not validated yet, so only a single parent is supported.
        if parents.len() != 1 {
            return Err(MononokeError::InvalidRequest(String::from(
                "Merge changesets and root changesets cannot be created",
            )));
        }

        // Obtain contexts for each of the parents (which should exist).
        let parent_ctxs: Vec<_> = parents
            .iter()
            .map(|parent_id| async move {
                let parent_ctx = self
                    .changeset(ChangesetSpecifier::Bonsai(parent_id.clone()))
                    .await?
                    .ok_or_else(|| {
                        MononokeError::InvalidRequest(format!(
                            "Parent {} does not exist",
                            parent_id
                        ))
                    })?;
                Ok::<_, MononokeError>(parent_ctx)
            })
            .collect::<FuturesOrdered<_>>()
            .try_collect()
            .await?;

        // Check that changes are valid according to bonsai rules:
        // (1) deletions and copy-from info must reference a real path in a
        //     valid parent.
        // (2) deletions for paths where a prefix directory has been replaced
        //     by a file should be dropped, as the deletion is implicit from the
        //     file change for the prefix path.
        // (3) conversely, when a file has been replaced by a directory, there
        //     must be a delete for the file.
        //

        // Extract the set of deleted files.
        let deleted_files: BTreeSet<_> = changes
            .iter()
            .filter(|(_path, change)| change.change_type() == CreateChangeType::Delete)
            .map(|(path, _change)| path.clone())
            .collect();

        // Check deleted files existed in a parent. (1)
        let fut_verify_deleted_files_existed = async {
            let (stats, result) =
                verify_deleted_files_existed_in_a_parent(&parent_ctxs, deleted_files)
                    .timed()
                    .await;
            let mut scuba = self.ctx().scuba().clone();
            scuba.add_future_stats(&stats);
            scuba.log_with_msg("Verify deleted files existed in a parent", None);
            result
        };

        // Build a path tree recording each path that has been created or deleted.
        let path_changes = PathTree::from_iter(
            changes
                .iter()
                .map(|(path, change)| (path.as_mpath().cloned(), change.change_type())),
        );

        // Determine the prefixes of all changed files.
        let prefix_paths: BTreeSet<_> = changes
            .iter()
            .filter(|(_path, change)| change.change_type() == CreateChangeType::Change)
            .map(|(path, _change)| path.clone().prefixes())
            .flatten()
            .collect();

        // Check changes that replace a directory with a file also delete
        // all files in that directory in all parents. (3)
        let fut_verify_prefix_files_deleted = async {
            let (stats, result) = parent_ctxs
                .iter()
                .map(|parent_ctx| {
                    verify_prefix_files_deleted(parent_ctx, &prefix_paths, &path_changes)
                })
                .collect::<FuturesUnordered<_>>()
                .try_for_each(|_| async { Ok(()) })
                .timed()
                .await;
            let mut scuba = self.ctx().scuba().clone();
            scuba.add_future_stats(&stats);
            scuba.log_with_msg("Verify prefix files in parents have been deleted", None);
            result
        };

        // Convert change paths into the form needed for the bonsai changeset.
        let changes: Vec<(MPath, CreateChange)> = changes
            .into_iter()
            // Filter deletions that have a change at a path prefix. The
            // deletion is implicit from the change. (2)
            .filter(|(path, change)| {
                change.change_type() != CreateChangeType::Delete
                    || !is_prefix_changed(path, &path_changes)
            })
            // Then convert the paths to MPaths. Do this before we start
            // resolving any changes, so that we don't start storing data
            // until we're happy that the changes are valid.
            .map(|(path, change)| {
                path.into_mpath()
                    .ok_or_else(|| {
                        MononokeError::InvalidRequest(String::from(
                            "Cannot create a file with an empty path",
                        ))
                    })
                    .map(move |mpath| (mpath, change))
            })
            .collect::<Result<_, _>>()?;

        // Resolve the changes into bonsai changes. This also checks (1) for
        // copy-from info.
        let file_changes_fut = async {
            let (stats, result) = changes
                .into_iter()
                .map(|(path, change)| {
                    let parent_ctxs = &parent_ctxs;
                    async move {
                        let change = change
                            .resolve(self.ctx().clone(), self.blob_repo(), &parent_ctxs)
                            .await?;
                        Ok::<_, MononokeError>((path, change))
                    }
                })
                .collect::<FuturesUnordered<_>>()
                .try_collect::<SortedVectorMap<MPath, Option<FileChange>>>()
                .timed()
                .await;
            let mut scuba = self.ctx().scuba().clone();
            scuba.add_future_stats(&stats);
            scuba.log_with_msg(
                "Convert create changeset parameters to bonsai changes",
                None,
            );
            result
        };

        let ((), (), file_changes) = try_join3(
            fut_verify_deleted_files_existed,
            fut_verify_prefix_files_deleted,
            file_changes_fut,
        )
        .await?;

        let author_date = MononokeDateTime::new(author_date);
        let committer_date = committer_date.map(MononokeDateTime::new);
        let extra = extra.into();

        // Create the new Bonsai Changeset. The `freeze` method validates
        // that the bonsai changeset is internally consistent.
        let new_changeset = BonsaiChangesetMut {
            parents,
            author,
            author_date,
            committer,
            committer_date,
            message,
            extra,
            file_changes,
        }
        .freeze()
        .map_err(|e| {
            MononokeError::InvalidRequest(format!("Changes create invalid bonsai changeset: {}", e))
        })?;

        let new_changeset_id = new_changeset.get_changeset_id();
        blobrepo::save_bonsai_changesets(
            vec![new_changeset],
            self.ctx().clone(),
            self.blob_repo().clone(),
        )
        .await?;
        Ok(ChangesetContext::new(self.repo.clone(), new_changeset_id))
    }
}
