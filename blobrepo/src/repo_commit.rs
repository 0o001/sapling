// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::mem;
use std::sync::{Arc, Mutex};

use failure::{err_msg, Compat, FutureFailureErrorExt};
use futures::IntoFuture;
use futures::future::{self, Future, Shared, SharedError, SharedItem};
use futures::stream::{self, Stream};
use futures::sync::oneshot;
use futures_ext::{BoxFuture, BoxStream, FutureExt, StreamExt};
use futures_stats::{Stats, Timed};
use slog::Logger;
use time_ext::DurationExt;
use uuid::Uuid;

use blobstore::Blobstore;
use filenodes::{FilenodeInfo, Filenodes};
use mercurial::{file, HgNodeKey, NodeHashConversion};
use mercurial_types::{Changeset, DChangesetId, DEntryId, DNodeHash, DParents, Entry, MPath,
                      Manifest, RepoPath, RepositoryId};
use mercurial_types::manifest::{self, Content};
use mercurial_types::manifest_utils::{changed_entry_stream, EntryStatus};
use mercurial_types::nodehash::{DFileNodeId, DManifestId};
use mononoke_types::DateTime;

use BlobChangeset;
use BlobRepo;
use changeset::ChangesetContent;
use errors::*;
use file::HgBlobEntry;
use utils::get_node_key;

/// A handle to a possibly incomplete BlobChangeset. This is used instead of
/// Future<Item = BlobChangeset> where we don't want to fully serialize waiting for completion.
/// For example, `create_changeset` takes these as p1/p2 so that it can handle the blobstore side
/// of creating a new changeset before its parent changesets are complete.
/// See `get_completed_changeset()` for the public API you can use to extract the final changeset
#[derive(Clone)]
pub struct ChangesetHandle {
    can_be_parent: Shared<oneshot::Receiver<(DNodeHash, DManifestId)>>,
    // * Shared is required here because a single changeset can have more than one child, and
    //   all of those children will want to refer to the corresponding future for their parents.
    // * The Compat<Error> here is because the error type for Shared (a cloneable wrapper called
    //   SharedError) doesn't implement Fail, and only implements Error if the wrapped type
    //   implements Error.
    completion_future: Shared<BoxFuture<BlobChangeset, Compat<Error>>>,
}

impl ChangesetHandle {
    pub fn new_pending(
        can_be_parent: Shared<oneshot::Receiver<(DNodeHash, DManifestId)>>,
        completion_future: Shared<BoxFuture<BlobChangeset, Compat<Error>>>,
    ) -> Self {
        Self {
            can_be_parent,
            completion_future,
        }
    }

    pub fn get_completed_changeset(self) -> Shared<BoxFuture<BlobChangeset, Compat<Error>>> {
        self.completion_future
    }
}

impl From<BlobChangeset> for ChangesetHandle {
    fn from(bcs: BlobChangeset) -> Self {
        let (trigger, can_be_parent) = oneshot::channel();
        // The send cannot fail at this point, barring an optimizer noticing that `can_be_parent`
        // is unused and dropping early. Eat the error, as in this case, nothing is blocked waiting
        // for the send
        let _ = trigger.send((bcs.get_changeset_id().into_nodehash(), *bcs.manifestid()));
        Self {
            can_be_parent: can_be_parent.shared(),
            completion_future: future::ok(bcs).boxify().shared(),
        }
    }
}

/// State used while tracking uploaded entries, to ensure that a changeset ends up with the right
/// set of blobs uploaded, and all filenodes present.
struct UploadEntriesState {
    /// Listing of blobs that we need, based on parsing the root manifest and all the newly
    /// uploaded child manifests
    required_entries: HashMap<RepoPath, DEntryId>,
    /// All the blobs that have been uploaded in this changeset
    uploaded_entries: HashMap<RepoPath, HgBlobEntry>,
    /// Parent hashes (if any) of the blobs that have been uploaded in this changeset. Used for
    /// validation of this upload - all parents must either have been uploaded in this changeset,
    /// or be present in the blobstore before the changeset can complete.
    parents: HashSet<HgNodeKey>,
    blobstore: Arc<Blobstore>,
    repoid: RepositoryId,
}

#[derive(Clone)]
pub struct UploadEntries {
    inner: Arc<Mutex<UploadEntriesState>>,
}

impl UploadEntries {
    pub fn new(blobstore: Arc<Blobstore>, repoid: RepositoryId) -> Self {
        Self {
            inner: Arc::new(Mutex::new(UploadEntriesState {
                required_entries: HashMap::new(),
                uploaded_entries: HashMap::new(),
                parents: HashSet::new(),
                blobstore,
                repoid,
            })),
        }
    }

    /// Parse a manifest and record the referenced blobs so that we know whether or not we have
    /// a complete changeset with all blobs, or whether there is missing data.
    fn process_manifest(&self, entry: &HgBlobEntry, path: RepoPath) -> BoxFuture<(), Error> {
        let inner_mutex = self.inner.clone();
        let parents_found = self.find_parents(entry, path.clone());
        let entry_hash = entry.get_hash().into_nodehash();
        let entry_type = entry.get_type();

        entry
            .get_content()
            .and_then(move |content| match content {
                Content::Tree(manifest) => manifest
                    .list()
                    .for_each(move |entry| {
                        let mpath = MPath::join_element_opt(path.mpath(), entry.get_name());
                        let mpath = match mpath {
                            Some(mpath) => mpath,
                            None => {
                                return future::err(err_msg(
                                    "internal error: unexpected empty MPath",
                                )).boxify()
                            }
                        };
                        let path = match entry.get_type() {
                            manifest::Type::File(_) => RepoPath::FilePath(mpath),
                            manifest::Type::Tree => RepoPath::DirectoryPath(mpath),
                        };
                        let mut inner = inner_mutex.lock().expect("Lock poisoned");
                        inner.required_entries.insert(path, *entry.get_hash());
                        future::ok(()).boxify()
                    })
                    .boxify(),
                _ => {
                    return future::err(ErrorKind::NotAManifest(entry_hash, entry_type).into())
                        .boxify()
                }
            })
            .join(parents_found)
            .map(|_| ())
            .boxify()
    }

    fn find_parents(&self, entry: &HgBlobEntry, path: RepoPath) -> BoxFuture<(), Error> {
        let inner_mutex = self.inner.clone();
        entry
            .get_parents()
            .and_then(move |parents| {
                let mut inner = inner_mutex.lock().expect("Lock poisoned");
                let node_keys = parents.into_iter().map(move |hash| HgNodeKey {
                    path: path.clone(),
                    hash: hash.into_mercurial(),
                });
                inner.parents.extend(node_keys);

                future::ok(())
            })
            .map(|_| ())
            .boxify()
    }

    /// The root manifest needs special processing - unlike all other entries, it is required even
    /// if no other manifest references it. Otherwise, this function is the same as
    /// `process_one_entry` and can be called after it.
    /// It is safe to call this multiple times, but not recommended - every manifest passed to
    /// this function is assumed required for this commit, even if it is not the root.
    pub fn process_root_manifest(&self, entry: &HgBlobEntry) -> BoxFuture<(), Error> {
        if entry.get_type() != manifest::Type::Tree {
            return future::err(
                ErrorKind::NotAManifest(entry.get_hash().into_nodehash(), entry.get_type()).into(),
            ).boxify();
        }
        {
            let mut inner = self.inner.lock().expect("Lock poisoned");
            inner
                .required_entries
                .insert(RepoPath::root(), *entry.get_hash());
        }
        self.process_one_entry(entry, RepoPath::root())
    }

    pub fn process_one_entry(&self, entry: &HgBlobEntry, path: RepoPath) -> BoxFuture<(), Error> {
        {
            let mut inner = self.inner.lock().expect("Lock poisoned");
            inner.uploaded_entries.insert(path.clone(), entry.clone());
        }
        if entry.get_type() == manifest::Type::Tree {
            self.process_manifest(entry, path)
        } else {
            self.find_parents(&entry, path)
        }
    }

    pub fn finalize(self, filenodes: Arc<Filenodes>, cs_id: DNodeHash) -> BoxFuture<(), Error> {
        let required_checks = {
            let inner = self.inner.lock().expect("Lock poisoned");
            let checks: Vec<_> = inner
                .required_entries
                .iter()
                .filter_map(|(path, entryid)| {
                    if inner.uploaded_entries.contains_key(path) {
                        None
                    } else {
                        let key = get_node_key(entryid.into_nodehash());
                        let blobstore = inner.blobstore.clone();
                        let path = path.clone();
                        Some(
                            blobstore
                                .assert_present(key)
                                .with_context(move |_| format!("While checking for path: {}", path))
                                .from_err(),
                        )
                    }
                })
                .collect();

            future::join_all(checks).boxify()
        };

        let parent_checks = {
            let inner = self.inner.lock().expect("Lock poisoned");
            let checks: Vec<_> = inner
                .parents
                .iter()
                .map(|node_key| {
                    let key = get_node_key(node_key.hash.into_mononoke());
                    let blobstore = inner.blobstore.clone();
                    let node_key = node_key.clone();
                    blobstore
                        .assert_present(key)
                        .with_context(move |_| {
                            format!("While checking for a parent node: {}", node_key)
                        })
                        .from_err()
                })
                .collect();

            future::join_all(checks).boxify()
        };

        let filenodes = {
            let mut inner = self.inner.lock().expect("Lock poisoned");
            let uploaded_entries = mem::replace(&mut inner.uploaded_entries, HashMap::new());
            let filenodeinfos = stream::iter_ok(uploaded_entries.into_iter())
                .and_then(|(path, blobentry): (_, HgBlobEntry)| {
                    blobentry
                        .get_parents()
                        .map(move |parents| (path, blobentry, parents))
                })
                .and_then(|(path, blobentry, parents)| {
                    let copyfrom = compute_copy_from_info(&path, &blobentry, &parents);
                    copyfrom.and_then(move |copyfrom| Ok((path, blobentry, parents, copyfrom)))
                })
                .map(move |(path, blobentry, parents, copyfrom)| {
                    let (p1, p2) = parents.get_nodes();
                    FilenodeInfo {
                        path,
                        filenode: DFileNodeId::new(blobentry.get_hash().into_nodehash()),
                        p1: p1.cloned().map(DFileNodeId::new),
                        p2: p2.cloned().map(DFileNodeId::new),
                        copyfrom,
                        linknode: DChangesetId::new(cs_id),
                    }
                })
                .boxify();

            filenodes.add_filenodes(filenodeinfos, &inner.repoid)
        };

        parent_checks
            .join3(required_checks, filenodes)
            .map(|_| ())
            .boxify()
    }
}

fn compute_copy_from_info(
    path: &RepoPath,
    blobentry: &HgBlobEntry,
    parents: &DParents,
) -> BoxFuture<Option<(RepoPath, DFileNodeId)>, Error> {
    let parents = parents.clone();
    match path {
        &RepoPath::FilePath(_) => blobentry
            .get_raw_content()
            .and_then({
                let parents = parents.clone();
                move |blob| {
                    // XXX this is broken -- parents.get_nodes() will never return
                    // (None, Some(hash)), which is what BlobNode relies on to figure out
                    // whether a node is copied.
                    let (p1, p2) = parents.get_nodes();
                    let p1 = p1.map(|p| p.into_mercurial());
                    let p2 = p2.map(|p| p.into_mercurial());
                    file::File::new(blob, p1.as_ref(), p2.as_ref())
                        .copied_from()
                        .map(|copiedfrom| {
                            copiedfrom.map(|(path, node)| {
                                (RepoPath::FilePath(path), DFileNodeId::new(node.into_mononoke()))
                            })
                        })
                }
            })
            .boxify(),
        &RepoPath::RootPath | &RepoPath::DirectoryPath(_) => {
            // No copy information for directories/repo roots
            Ok(None).into_future().boxify()
        }
    }
}

fn compute_changed_files_pair(
    to: &Box<Manifest + Sync>,
    from: &Box<Manifest + Sync>,
) -> BoxFuture<HashSet<MPath>, Error> {
    changed_entry_stream(to, from, None)
        .filter_map(|change| match change.status {
            EntryStatus::Deleted(entry)
            | EntryStatus::Added(entry)
            | EntryStatus::Modified {
                to_entry: entry, ..
            } => {
                if entry.get_type() == manifest::Type::Tree {
                    None
                } else {
                    MPath::join_element_opt(change.path.as_ref(), entry.get_name())
                }
            }
        })
        .fold(HashSet::new(), |mut set, path| {
            set.insert(path);
            future::ok::<_, Error>(set)
        })
        .boxify()
}

pub fn compute_changed_files(
    root: &Box<Manifest + Sync>,
    p1: Option<&Box<Manifest + Sync>>,
    p2: Option<&Box<Manifest + Sync>>,
) -> BoxFuture<Vec<MPath>, Error> {
    let empty = manifest::EmptyManifest {}.boxed();
    match (p1, p2) {
        (None, None) => compute_changed_files_pair(&root, &empty),
        (Some(manifest), None) | (None, Some(manifest)) => {
            compute_changed_files_pair(&root, &manifest)
        }
        (Some(p1), Some(p2)) => compute_changed_files_pair(&root, &p1)
            .join(compute_changed_files_pair(&root, &p2))
            .map(|(left, right)| {
                left.symmetric_difference(&right)
                    .cloned()
                    .collect::<HashSet<MPath>>()
            })
            .boxify(),
    }.map(|files| {
        let mut files: Vec<MPath> = files.into_iter().collect();
        files.sort_unstable();

        files
    })
        .boxify()
}

pub fn process_entries(
    logger: Logger,
    uuid: Uuid,
    repo: BlobRepo,
    entry_processor: &UploadEntries,
    root_manifest: BoxFuture<(HgBlobEntry, RepoPath), Error>,
    new_child_entries: BoxStream<(HgBlobEntry, RepoPath), Error>,
) -> BoxFuture<(Box<Manifest + Sync>, DManifestId), Error> {
    root_manifest
        .and_then({
            let entry_processor = entry_processor.clone();
            move |(entry, path)| {
                let hash = entry.get_hash().into_nodehash();
                if entry.get_type() == manifest::Type::Tree && path == RepoPath::RootPath {
                    entry_processor
                        .process_root_manifest(&entry)
                        .map(move |_| hash)
                        .boxify()
                } else {
                    future::err(Error::from(ErrorKind::BadRootManifest(entry.get_type()))).boxify()
                }
            }
        })
        .and_then({
            let entry_processor = entry_processor.clone();
            |hash| {
                new_child_entries
                    .for_each(move |(entry, path)| entry_processor.process_one_entry(&entry, path))
                    .map(move |_| hash)
            }
        })
        .and_then(move |root_hash| {
            repo.get_manifest_by_nodeid(&root_hash)
                .map(move |m| (m, DManifestId::new(root_hash)))
        })
        .timed(move |stats, result| {
            if result.is_ok() {
                log_cs_future_stats(&logger, "upload_entries", stats, uuid);
            }
            Ok(())
        })
        .boxify()
}

pub fn log_cs_future_stats(logger: &Logger, phase: &str, stats: Stats, uuid: Uuid) {
    let uuid = format!("{}", uuid);
    debug!(logger, "Changeset creation";
        "changeset_uuid" => uuid,
        "phase" => String::from(phase),
        "poll_count" => stats.poll_count,
        "poll_time_us" => stats.poll_time.as_micros_unchecked(),
        "completion_time_us" => stats.completion_time.as_micros_unchecked(),
    );
}

pub fn extract_parents_complete(
    p1: &Option<ChangesetHandle>,
    p2: &Option<ChangesetHandle>,
) -> BoxFuture<SharedItem<()>, SharedError<Compat<Error>>> {
    match (p1.as_ref(), p2.as_ref()) {
        (None, None) => future::ok(()).shared().boxify(),
        (Some(p), None) | (None, Some(p)) => p.completion_future
            .clone()
            .and_then(|_| future::ok(()).shared())
            .boxify(),
        (Some(p1), Some(p2)) => p1.completion_future
            .clone()
            .join(p2.completion_future.clone())
            .and_then(|_| future::ok(()).shared())
            .boxify(),
    }.boxify()
}

pub fn handle_parents(
    logger: Logger,
    uuid: Uuid,
    repo: BlobRepo,
    p1: Option<ChangesetHandle>,
    p2: Option<ChangesetHandle>,
) -> BoxFuture<
    (
        DParents,
        (Option<Box<Manifest + Sync>>),
        (Option<Box<Manifest + Sync>>),
    ),
    Error,
> {
    let p1 = p1.map(|cs| cs.can_be_parent);
    let p2 = p2.map(|cs| cs.can_be_parent);
    p1.join(p2)
        .and_then(|(p1, p2)| {
            let p1 = match p1 {
                Some(item) => {
                    let (hash, manifest) = *item;
                    (Some(hash), Some(manifest))
                }
                None => (None, None),
            };
            let p2 = match p2 {
                Some(item) => {
                    let (hash, manifest) = *item;
                    (Some(hash), Some(manifest))
                }
                None => (None, None),
            };
            future::ok((p1, p2))
        })
        .map_err(|e| Error::from(e))
        .and_then(move |((p1_hash, p1_manifest), (p2_hash, p2_manifest))| {
            let parents = DParents::new(p1_hash.as_ref(), p2_hash.as_ref());
            let p1_manifest = p1_manifest.map(|m| repo.get_manifest_by_nodeid(&m.into_nodehash()));
            let p2_manifest = p2_manifest.map(|m| repo.get_manifest_by_nodeid(&m.into_nodehash()));
            p1_manifest
                .join(p2_manifest)
                .map(move |(p1_manifest, p2_manifest)| (parents, p1_manifest, p2_manifest))
        })
        .timed(move |stats, result| {
            if result.is_ok() {
                log_cs_future_stats(&logger, "wait_for_parents_ready", stats, uuid);
            }
            Ok(())
        })
        .boxify()
}

pub fn make_new_changeset(
    parents: DParents,
    root_hash: DManifestId,
    user: String,
    time: DateTime,
    extra: BTreeMap<Vec<u8>, Vec<u8>>,
    files: Vec<MPath>,
    comments: String,
) -> Result<BlobChangeset> {
    let changeset = ChangesetContent::new_from_parts(
        parents,
        root_hash,
        user.into_bytes(),
        time,
        extra,
        files,
        comments.into_bytes(),
    );
    BlobChangeset::new(changeset)
}
