/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::BTreeMap;

use anyhow::Error;
use ascii::AsAsciiStr;
use bytes::Bytes;
use fbinit::FacebookInit;
use futures_ext::{BoxFuture, StreamExt};
use futures_old::stream::futures_unordered;
use scuba_ext::ScubaSampleBuilder;

use blobrepo::BlobRepo;
use blobrepo_factory::new_memblob_empty;
use blobrepo_hg::{ChangesetHandle, CreateChangeset};
use context::CoreContext;
use memblob::Memblob;
use mercurial_types::{
    blobs::{
        ChangesetMetadata, HgBlobEntry, UploadHgFileContents, UploadHgFileEntry, UploadHgNodeHash,
        UploadHgTreeEntry,
    },
    FileType, HgBlobNode, HgFileNodeId, HgNodeHash, MPath, RepoPath,
};
use mononoke_types::DateTime;
use std::sync::Arc;

pub fn get_empty_repo() -> BlobRepo {
    new_memblob_empty(Some(Arc::new(Memblob::default()))).expect("cannot create empty repo")
}

pub fn upload_file_no_parents<B>(
    ctx: CoreContext,
    repo: &BlobRepo,
    data: B,
    path: &RepoPath,
) -> (HgFileNodeId, BoxFuture<(HgBlobEntry, RepoPath), Error>)
where
    B: Into<Bytes>,
{
    upload_hg_file_entry(
        ctx,
        repo,
        data.into(),
        FileType::Regular,
        path.clone(),
        None,
        None,
    )
}

pub fn upload_file_one_parent<B>(
    ctx: CoreContext,
    repo: &BlobRepo,
    data: B,
    path: &RepoPath,
    p1: HgFileNodeId,
) -> (HgFileNodeId, BoxFuture<(HgBlobEntry, RepoPath), Error>)
where
    B: Into<Bytes>,
{
    upload_hg_file_entry(
        ctx,
        repo,
        data.into(),
        FileType::Regular,
        path.clone(),
        Some(p1),
        None,
    )
}

pub fn upload_manifest_no_parents<B>(
    ctx: CoreContext,
    repo: &BlobRepo,
    data: B,
    path: &RepoPath,
) -> (HgNodeHash, BoxFuture<(HgBlobEntry, RepoPath), Error>)
where
    B: Into<Bytes>,
{
    upload_hg_tree_entry(ctx, repo, data.into(), path.clone(), None, None)
}

pub fn upload_manifest_one_parent<B>(
    ctx: CoreContext,
    repo: &BlobRepo,
    data: B,
    path: &RepoPath,
    p1: HgNodeHash,
) -> (HgNodeHash, BoxFuture<(HgBlobEntry, RepoPath), Error>)
where
    B: Into<Bytes>,
{
    upload_hg_tree_entry(ctx, repo, data.into(), path.clone(), Some(p1), None)
}

fn upload_hg_tree_entry(
    ctx: CoreContext,
    repo: &BlobRepo,
    contents: Bytes,
    path: RepoPath,
    p1: Option<HgNodeHash>,
    p2: Option<HgNodeHash>,
) -> (HgNodeHash, BoxFuture<(HgBlobEntry, RepoPath), Error>) {
    let upload = UploadHgTreeEntry {
        upload_node_id: UploadHgNodeHash::Generate,
        contents,
        p1,
        p2,
        path,
    };
    upload.upload(ctx, repo.get_blobstore().boxed()).unwrap()
}

fn upload_hg_file_entry(
    ctx: CoreContext,
    repo: &BlobRepo,
    contents: Bytes,
    file_type: FileType,
    path: RepoPath,
    p1: Option<HgFileNodeId>,
    p2: Option<HgFileNodeId>,
) -> (HgFileNodeId, BoxFuture<(HgBlobEntry, RepoPath), Error>) {
    // Ideally the node id returned from upload.upload would be used, but that isn't immediately
    // available -- so compute it ourselves.
    let node_id = HgBlobNode::new(
        contents.clone(),
        p1.map(HgFileNodeId::into_nodehash),
        p2.map(HgFileNodeId::into_nodehash),
    )
    .nodeid();

    let upload = UploadHgFileEntry {
        upload_node_id: UploadHgNodeHash::Checked(node_id),
        contents: UploadHgFileContents::RawBytes(contents, repo.filestore_config()),
        file_type,
        p1,
        p2,
        path: path.into_mpath().expect("expected a path to be present"),
    };

    let (_, upload_fut) = upload.upload(ctx, repo.get_blobstore().boxed()).unwrap();
    (HgFileNodeId::new(node_id), upload_fut)
}

pub fn create_changeset_no_parents(
    fb: FacebookInit,
    repo: &BlobRepo,
    root_manifest: BoxFuture<Option<(HgBlobEntry, RepoPath)>, Error>,
    other_nodes: Vec<BoxFuture<(HgBlobEntry, RepoPath), Error>>,
) -> ChangesetHandle {
    let cs_metadata = ChangesetMetadata {
        user: "author <author@fb.com>".into(),
        time: DateTime::from_timestamp(0, 0).expect("valid timestamp"),
        extra: BTreeMap::new(),
        message: "Test commit".into(),
    };
    let create_changeset = CreateChangeset {
        expected_nodeid: None,
        expected_files: None,
        p1: None,
        p2: None,
        root_manifest,
        sub_entries: futures_unordered(other_nodes).boxify(),
        cs_metadata,
        must_check_case_conflicts: true,
        create_bonsai_changeset_hook: None,
    };
    create_changeset.create(
        CoreContext::test_mock(fb),
        repo,
        ScubaSampleBuilder::with_discard(),
    )
}

pub fn create_changeset_one_parent(
    fb: FacebookInit,
    repo: &BlobRepo,
    root_manifest: BoxFuture<Option<(HgBlobEntry, RepoPath)>, Error>,
    other_nodes: Vec<BoxFuture<(HgBlobEntry, RepoPath), Error>>,
    p1: ChangesetHandle,
) -> ChangesetHandle {
    let cs_metadata = ChangesetMetadata {
        user: "\u{041F}\u{0451}\u{0442}\u{0440} <peter@fb.com>".into(),
        time: DateTime::from_timestamp(1234, 0).expect("valid timestamp"),
        extra: BTreeMap::new(),
        message: "Child commit".into(),
    };
    let create_changeset = CreateChangeset {
        expected_nodeid: None,
        expected_files: None,
        p1: Some(p1),
        p2: None,
        root_manifest,
        sub_entries: futures_unordered(other_nodes).boxify(),
        cs_metadata,
        must_check_case_conflicts: true,
        create_bonsai_changeset_hook: None,
    };
    create_changeset.create(
        CoreContext::test_mock(fb),
        repo,
        ScubaSampleBuilder::with_discard(),
    )
}

pub fn string_to_nodehash(hash: &str) -> HgNodeHash {
    HgNodeHash::from_ascii_str(hash.as_ascii_str().unwrap()).unwrap()
}

pub fn to_mpath(path: RepoPath) -> Result<MPath, Error> {
    let bad_mpath = Error::msg("RepoPath did not convert to MPath");
    path.into_mpath().ok_or(bad_mpath)
}
