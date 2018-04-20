// Copyright (c) 2017-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

#![allow(deprecated)]

use std::sync::Arc;
use std::sync::mpsc::SyncSender;

use failure::StreamFailureErrorExt;
use futures::{Future, IntoFuture, Stream};
use futures_cpupool::CpuPool;
use slog::Logger;
use tokio_core::reactor::Core;

use blobrepo::{BlobChangeset, ChangesetContent};
use changesets::{ChangesetInsert, Changesets};
use failure::{Error, Result};
use filenodes::FilenodeInfo;
use futures::sync::mpsc::UnboundedSender;
use futures_ext::{BoxFuture, BoxStream, FutureExt, StreamExt};
use heads::Heads;
use mercurial::{self, HgChangesetId, HgEntryId, HgNodeHash, HgParents, RevlogManifest, RevlogRepo};
use mercurial::revlog::RevIdx;
use mercurial::revlogrepo::RevlogRepoBlobimportExt;
use mercurial_types::{DFileNodeId, HgBlob, RepoPath, RepositoryId};
use mercurial_types::nodehash::DChangesetId;
use stats::Timeseries;

use BlobstoreEntry;
use STATS;
use manifest;

pub(crate) struct ConvertContext<H> {
    pub repo: RevlogRepo,
    pub headstore: H,
    pub core: Core,
    pub cpupool: Arc<CpuPool>,
    pub logger: Logger,
    pub skip: Option<u64>,
    pub commits_limit: Option<u64>,
}

impl<H> ConvertContext<H>
where
    H: Heads,
{
    pub fn convert(
        &mut self,
        sender: SyncSender<BlobstoreEntry>,
        filenodes_sender: UnboundedSender<FilenodeInfo>,
    ) -> Result<()> {
        let core = &mut self.core;
        let logger = &self.logger.clone();
        let cpupool = self.cpupool.clone();
        let headstore = &self.headstore;
        let skip = self.skip;
        let commits_limit = self.commits_limit;

        let changesets: BoxStream<HgNodeHash, mercurial::Error> = if let Some(skip) = skip {
            self.repo.changesets().skip(skip).boxify()
        } else {
            self.repo.changesets().boxify()
        };

        let changesets: BoxStream<HgNodeHash, mercurial::Error> = if let Some(limit) = commits_limit
        {
            changesets.take(limit).boxify()
        } else {
            changesets.boxify()
        };

        // Generate stream of changesets. For each changeset, save the cs blob, and the manifest
        // blob, and the files.
        let changesets = changesets
            .map_err(Error::from)
            .enumerate()
            .map({
                let repo = self.repo.clone();
                let sender = sender.clone();
                move |(seq, csid)| {
                    debug!(logger, "{}: changeset {}", seq, csid);
                    STATS::changesets.add_value(1);
                    copy_changeset(
                        repo.clone(),
                        sender.clone(),
                        filenodes_sender.clone(),
                        HgChangesetId::new(csid)
                    )
                }
            }) // Stream<Future<()>>
            .map(|copy| cpupool.spawn(copy))
            .buffer_unordered(100);

        let heads = self.repo
            .get_heads()
            .context("Failed get heads")
            .from_err()
            .map(|h| {
                debug!(logger, "head {}", h);
                STATS::heads.add_value(1);
                let h = h.into_mononoke();
                headstore.add(&h).map_err({
                    move |err| {
                        err.context(format_err!("Failed to create head {}", h))
                            .into()
                    }
                })
            })
            .buffer_unordered(100);

        let convert = changesets.select(heads).for_each(|_| Ok(()));

        core.run(convert)?;

        info!(logger, "parsed everything, waiting for io");
        Ok(())
    }

    pub fn fill_changesets_store(
        &self,
        changesets_store: Arc<Changesets>,
        repo_id: &RepositoryId,
    ) -> BoxFuture<(), mercurial::Error> {
        let repo = self.repo.clone();
        let repo_id = *repo_id;
        self.get_changesets_stream()
            .and_then(move |node| {
                repo.get_changeset(&HgChangesetId::new(node))
                    .map(move |cs| (cs, node))
            })
            .for_each(move |(cs, node)| {
                let parents = cs.parents()
                    .into_iter()
                    .map(|p| DChangesetId::new(p.into_mononoke()))
                    .collect();
                let node = node.into_mononoke();
                let insert = ChangesetInsert {
                    repo_id,
                    cs_id: DChangesetId::new(node),
                    parents,
                };
                changesets_store.add(insert)
            })
            .boxify()
    }

    fn get_changesets_stream(&self) -> BoxStream<HgNodeHash, mercurial::Error> {
        let changesets: BoxStream<HgNodeHash, mercurial::Error> = if let Some(skip) = self.skip {
            self.repo.changesets().skip(skip).boxify()
        } else {
            self.repo.changesets().boxify()
        };

        if let Some(limit) = self.commits_limit {
            changesets.take(limit).boxify()
        } else {
            changesets.boxify()
        }
    }
}

/// Copy a changeset and its manifest into the blobstore
///
/// The changeset and the manifest are straightforward - we just make literal copies of the
/// blobs into the blobstore.
///
/// The files are more complex. For each manifest, we generate a stream of entries, then flatten
/// the entry streams from all changesets into a single stream. Then each entry is filtered
/// against a set of entries that have already been copied, and any remaining are actually copied.
fn copy_changeset(
    revlog_repo: RevlogRepo,
    sender: SyncSender<BlobstoreEntry>,
    filenodes: UnboundedSender<FilenodeInfo>,
    csid: HgChangesetId,
) -> impl Future<Item = (), Error = Error> + Send + 'static
where
    Error: Send + 'static,
{
    let put = {
        let sender = sender.clone();
        let csid = csid;

        revlog_repo
            .get_changeset(&csid)
            .from_err()
            .and_then(move |cs| {
                let csid = DChangesetId::new(csid.into_nodehash().into_mononoke());
                let bcs = BlobChangeset::new_with_id(&csid, ChangesetContent::from_revlogcs(cs));
                sender
                    .send(BlobstoreEntry::Changeset(bcs))
                    .map_err(Error::from)
            })
    };

    let nodeid = csid.clone().into_nodehash();
    let entryid = HgEntryId::new(nodeid);
    let manifest = revlog_repo
        .get_changeset(&csid)
        .join(revlog_repo.get_changelog_entry_by_id(&entryid))
        .from_err()
        .and_then(move |(cs, entry)| {
            let mfid = *cs.manifestid();
            let linkrev = entry.linkrev;
            put_blobs(
                revlog_repo,
                sender,
                filenodes,
                mfid.clone().into_nodehash(),
                linkrev,
            )
        })
        .map_err(move |err| {
            err.context(format_err!("Can't copy manifest for cs {}", csid))
                .into()
        });
    _assert_sized(&put);
    _assert_sized(&manifest);

    put.join(manifest).map(|_| ())
}

/// Copy manifest and filelog entries into the blob store.
///
/// See the help for copy_changeset for a full description.
fn put_blobs(
    revlog_repo: RevlogRepo,
    sender: SyncSender<BlobstoreEntry>,
    filenodes: UnboundedSender<FilenodeInfo>,
    mfid: HgNodeHash,
    linkrev: RevIdx,
) -> impl Future<Item = (), Error = Error> + Send + 'static {
    let cs_entry_fut = revlog_repo
        .get_changelog_entry_by_idx(linkrev)
        .into_future();

    revlog_repo
        .get_manifest_blob_by_id(&mfid)
        .into_future()
        .join(cs_entry_fut)
        .from_err()
        .and_then(move |(rootmfblob, cs_entry)| {
            let putmf = manifest::put_entry(
                sender.clone(),
                mfid,
                rootmfblob.as_blob().clone(),
                rootmfblob.parents().clone(),
            );

            let linknode = cs_entry.nodeid;
            let filenode = create_filenode(
                rootmfblob.as_blob().clone(),
                mfid,
                *rootmfblob.parents(),
                RepoPath::RootPath,
                linknode,
            );

            filenodes
                .unbounded_send(filenode)
                .expect("failed to send root filenodeinfo");
            // Get the listing of entries and fetch each of those
            let files = RevlogManifest::new(revlog_repo.clone(), rootmfblob)
                .map_err(|err| Error::from(err.context("Parsing manifest to get list")))
                .map(|mf| mf.list().map_err(Error::from))
                .map(|entry_stream| {
                    entry_stream
                        .map({
                            let revlog_repo = revlog_repo.clone();
                            move |entry| {
                                manifest::get_entry_stream(
                                    entry,
                                    revlog_repo.clone(),
                                    linkrev.clone(),
                                    None,
                                )
                            }
                        })
                        .flatten()
                        .and_then(|(entry, repopath)| {
                            entry
                                .get_parents()
                                .join(entry.get_raw_content())
                                .map(move |(parents, blob)| (entry, blob, repopath, parents))
                        })
                        .for_each(move |(entry, blob, repopath, parents)| {
                            // All entries share the same linknode to the changelog.
                            let filenode_hash = entry.get_hash().clone();
                            let filenode = create_filenode(
                                blob,
                                filenode_hash.into_nodehash(),
                                parents,
                                repopath,
                                linknode,
                            );
                            filenodes
                                .unbounded_send(filenode)
                                .expect("failed to send filenodeinfo");
                            let copy_future = manifest::copy_entry(entry, sender.clone());
                            copy_future.map(|_| ())
                        })
                })
                .into_future()
                .flatten();

            _assert_sized(&files);
            // Huh? No idea why this is needed to avoid an error below.
            let files = files.boxify();

            putmf.join(files).map(|_| ())
        })
}

fn create_filenode(
    blob: HgBlob,
    filenode_hash: HgNodeHash,
    parents: HgParents,
    repopath: RepoPath,
    linknode: HgNodeHash,
) -> FilenodeInfo {
    let (p1, p2) = parents.get_nodes();
    let copyfrom = mercurial::file::File::new(blob, p1, p2)
        .copied_from()
        .map(|copiedfrom| {
            copiedfrom.map(|(path, node)| {
                (
                    RepoPath::FilePath(path),
                    DFileNodeId::new(node.into_mononoke()),
                )
            })
        })
        .expect("cannot create filenode");

    FilenodeInfo {
        path: repopath.clone(),
        filenode: DFileNodeId::new(filenode_hash.into_mononoke()),
        p1: p1.map(|p| DFileNodeId::new(p.into_mononoke())),
        p2: p2.map(|p| DFileNodeId::new(p.into_mononoke())),
        copyfrom,
        linknode: DChangesetId::new(linknode.into_mononoke()),
    }
}

fn _assert_sized<T: Sized>(_: &T) {}
