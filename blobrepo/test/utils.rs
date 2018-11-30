// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::collections::BTreeMap;

use ascii::AsAsciiStr;
use bytes::Bytes;
use failure::Error;
use futures::executor::spawn;
use futures::future::Future;
use futures::stream::futures_unordered;
use futures_ext::{BoxFuture, StreamExt};
use scuba_ext::ScubaSampleBuilder;

use blobrepo::{BlobRepo, ChangesetHandle, ChangesetMetadata, CreateChangeset, HgBlobEntry,
               UploadHgFileContents, UploadHgFileEntry, UploadHgNodeHash, UploadHgTreeEntry};
use blobstore::{EagerMemblob, LazyMemblob};
use context::CoreContext;
use mercurial_types::{FileType, HgBlobNode, HgNodeHash, RepoPath};
use mononoke_types::DateTime;
use std::sync::Arc;

pub fn get_empty_eager_repo() -> BlobRepo {
    BlobRepo::new_memblob_empty(None, Some(Arc::new(EagerMemblob::new())))
        .expect("cannot create empty repo")
}

pub fn get_empty_lazy_repo() -> BlobRepo {
    BlobRepo::new_memblob_empty(None, Some(Arc::new(LazyMemblob::new())))
        .expect("cannot create empty repo")
}

macro_rules! test_both_repotypes {
    ($impl_name:ident, $lazy_test:ident, $eager_test:ident) => {
        #[test]
        fn $lazy_test() {
            async_unit::tokio_unit_test(|| {
                $impl_name(get_empty_lazy_repo());
            })
        }

        #[test]
        fn $eager_test() {
            async_unit::tokio_unit_test(|| {
                $impl_name(get_empty_eager_repo());
            })
        }
    };
    (should_panic, $impl_name:ident, $lazy_test:ident, $eager_test:ident) => {
        #[test]
        #[should_panic]
        fn $lazy_test() {
            async_unit::tokio_unit_test(|| {
                $impl_name(get_empty_lazy_repo());
            })
        }

        #[test]
        #[should_panic]
        fn $eager_test() {
            async_unit::tokio_unit_test(|| {
                $impl_name(get_empty_eager_repo());
            })
        }
    }
}

pub fn upload_file_no_parents<B>(
    repo: &BlobRepo,
    data: B,
    path: &RepoPath,
) -> (HgNodeHash, BoxFuture<(HgBlobEntry, RepoPath), Error>)
where
    B: Into<Bytes>,
{
    upload_hg_file_entry(
        repo,
        data.into(),
        FileType::Regular,
        path.clone(),
        None,
        None,
    )
}

pub fn upload_file_one_parent<B>(
    repo: &BlobRepo,
    data: B,
    path: &RepoPath,
    p1: HgNodeHash,
) -> (HgNodeHash, BoxFuture<(HgBlobEntry, RepoPath), Error>)
where
    B: Into<Bytes>,
{
    upload_hg_file_entry(
        repo,
        data.into(),
        FileType::Regular,
        path.clone(),
        Some(p1),
        None,
    )
}

pub fn upload_manifest_no_parents<B>(
    repo: &BlobRepo,
    data: B,
    path: &RepoPath,
) -> (HgNodeHash, BoxFuture<(HgBlobEntry, RepoPath), Error>)
where
    B: Into<Bytes>,
{
    upload_hg_tree_entry(repo, data.into(), path.clone(), None, None)
}

pub fn upload_manifest_one_parent<B>(
    repo: &BlobRepo,
    data: B,
    path: &RepoPath,
    p1: HgNodeHash,
) -> (HgNodeHash, BoxFuture<(HgBlobEntry, RepoPath), Error>)
where
    B: Into<Bytes>,
{
    upload_hg_tree_entry(repo, data.into(), path.clone(), Some(p1), None)
}

fn upload_hg_tree_entry(
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
    upload.upload(repo).unwrap()
}

fn upload_hg_file_entry(
    repo: &BlobRepo,
    contents: Bytes,
    file_type: FileType,
    path: RepoPath,
    p1: Option<HgNodeHash>,
    p2: Option<HgNodeHash>,
) -> (HgNodeHash, BoxFuture<(HgBlobEntry, RepoPath), Error>) {
    // Ideally the node id returned from upload.upload would be used, but that isn't immediately
    // available -- so compute it ourselves.
    let node_id = HgBlobNode::new(contents.clone(), p1.as_ref(), p2.as_ref()).nodeid();

    let upload = UploadHgFileEntry {
        upload_node_id: UploadHgNodeHash::Checked(node_id),
        contents: UploadHgFileContents::RawBytes(contents),
        file_type,
        p1,
        p2,
        path: path.into_mpath().expect("expected a path to be present"),
    };

    let (_, upload_fut) = upload.upload(repo).unwrap();
    (node_id, upload_fut)
}

pub fn create_changeset_no_parents(
    repo: &BlobRepo,
    root_manifest: BoxFuture<Option<(HgBlobEntry, RepoPath)>, Error>,
    other_nodes: Vec<BoxFuture<(HgBlobEntry, RepoPath), Error>>,
) -> ChangesetHandle {
    let cs_metadata = ChangesetMetadata {
        user: "author <author@fb.com>".into(),
        time: DateTime::from_timestamp(0, 0).expect("valid timestamp"),
        extra: BTreeMap::new(),
        comments: "Test commit".into(),
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
    };
    create_changeset.create(
        CoreContext::test_mock(),
        repo,
        ScubaSampleBuilder::with_discard(),
    )
}

pub fn create_changeset_one_parent(
    repo: &BlobRepo,
    root_manifest: BoxFuture<Option<(HgBlobEntry, RepoPath)>, Error>,
    other_nodes: Vec<BoxFuture<(HgBlobEntry, RepoPath), Error>>,
    p1: ChangesetHandle,
) -> ChangesetHandle {
    let cs_metadata = ChangesetMetadata {
        user: "\u{041F}\u{0451}\u{0442}\u{0440} <peter@fb.com>".into(),
        time: DateTime::from_timestamp(1234, 0).expect("valid timestamp"),
        extra: BTreeMap::new(),
        comments: "Child commit".into(),
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
    };
    create_changeset.create(
        CoreContext::test_mock(),
        repo,
        ScubaSampleBuilder::with_discard(),
    )
}

pub fn string_to_nodehash(hash: &str) -> HgNodeHash {
    HgNodeHash::from_ascii_str(hash.as_ascii_str().unwrap()).unwrap()
}

pub fn run_future<F>(future: F) -> Result<F::Item, F::Error>
where
    F: Future,
{
    spawn(future).wait_future()
}
