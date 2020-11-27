/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

mod file_history_test;
mod tracing_blobstore;
mod utils;

use ::manifest::{Entry, Manifest, ManifestOps};
use anyhow::Error;
use assert_matches::assert_matches;
use blobrepo::BlobRepo;
use blobrepo_errors::ErrorKind;
use blobrepo_hg::{
    repo_commit::{check_case_conflicts, compute_changed_files, UploadEntries},
    BlobRepoHg,
};
use blobstore::{Loadable, Storable};
use bytes::Bytes;
use cloned::cloned;
use context::CoreContext;
use fbinit::FacebookInit;
use fixtures::{create_bonsai_changeset, many_files_dirs, merge_uneven};
use futures::{compat::Future01CompatExt, FutureExt, TryFutureExt};
use futures_ext::{BoxFuture, FutureExt as _};
use futures_old::Future;
use maplit::btreemap;
use memblob::Memblob;
use mercurial_derived_data::get_manifest_from_bonsai;
use mercurial_types::{
    blobs::{
        ContentBlobMeta, File, HgBlobChangeset, HgBlobManifest, UploadHgFileContents,
        UploadHgFileEntry, UploadHgNodeHash,
    },
    FileType, HgChangesetId, HgFileEnvelope, HgFileNodeId, HgManifestId, HgNodeHash, HgParents,
    MPath, RepoPath,
};
use mercurial_types_mocks::nodehash::ONES_FNID;
use mononoke_types::bonsai_changeset::BonsaiChangesetMut;
use mononoke_types::{
    blob::BlobstoreValue, BonsaiChangeset, ChangesetId, DateTime, FileChange, FileContents,
};
use scuba_ext::ScubaSampleBuilder;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};
use tests_utils::{create_commit, store_files, CreateCommitContext};
use tracing_blobstore::TracingBlobstore;
use utils::{
    create_changeset_no_parents, create_changeset_one_parent, get_empty_repo, string_to_nodehash,
    to_leaf, to_mpath, to_tree, upload_file_no_parents, upload_file_one_parent,
    upload_manifest_no_parents, upload_manifest_one_parent,
};

async fn get_content(
    ctx: &CoreContext,
    repo: &BlobRepo,
    id: HgFileNodeId,
) -> Result<bytes::Bytes, Error> {
    let content_id = id.load(ctx, repo.blobstore()).await?.content_id();
    let content = filestore::fetch_concat(repo.blobstore(), ctx, content_id);
    content.await
}

#[fbinit::compat_test]
async fn upload_blob_no_parents(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();
    let expected_hash = HgFileNodeId::new(string_to_nodehash(
        "c3127cdbf2eae0f09653f9237d85c8436425b246",
    ));
    let fake_path = RepoPath::file("fake/file").expect("Can't generate fake RepoPath");

    // The blob does not exist...
    assert!(get_content(&ctx, &repo, expected_hash).await.is_err());

    // We upload it...
    let (fnid, future) = upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_path);
    assert!(fnid == expected_hash);

    // The entry we're given is correct...
    let (fnid, path) = future.compat().await.unwrap();
    assert!(path == fake_path);
    assert!(fnid == expected_hash);

    // And the blob now exists
    let bytes = get_content(&ctx, &repo, expected_hash).await.unwrap();
    assert!(bytes.as_ref() == &b"blob"[..]);
}

#[fbinit::compat_test]
async fn upload_blob_one_parent(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();
    let expected_hash = HgFileNodeId::new(string_to_nodehash(
        "c2d60b35a8e7e034042a9467783bbdac88a0d219",
    ));
    let fake_path = RepoPath::file("fake/file").expect("Can't generate fake RepoPath");

    let (p1, future) = upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_path);

    // The blob does not exist...
    let _ = get_content(&ctx, &repo, expected_hash).await.unwrap_err();

    // We upload it...
    let (hash, future2) = upload_file_one_parent(ctx.clone(), &repo, "blob", &fake_path, p1);
    assert!(hash == expected_hash);

    // The entry we're given is correct...
    let (fnid, path) = future2
        .join(future)
        .map(|(item, _)| item)
        .compat()
        .await
        .unwrap();

    assert!(path == fake_path);
    assert!(fnid == expected_hash);

    // And the blob now exists
    let bytes = get_content(&ctx, &repo, expected_hash).await.unwrap();
    assert!(bytes.as_ref() == &b"blob"[..]);
}

#[fbinit::compat_test]
async fn create_one_changeset(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();
    let fake_file_path = RepoPath::file("dir/file").expect("Can't generate fake RepoPath");
    let fake_dir_path = RepoPath::dir("dir").expect("Can't generate fake RepoPath");
    let expected_files = vec![
        RepoPath::file("dir/file")
            .expect("Can't generate fake RepoPath")
            .mpath()
            .unwrap()
            .clone(),
    ];
    let author: String = "author <author@fb.com>".into();

    let (filehash, file_future) =
        upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path);

    let (dirhash, manifest_dir_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("file\0{}\n", filehash),
        &fake_dir_path,
    );

    let (root_mfid, root_manifest_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("dir\0{}t\n", dirhash),
        &RepoPath::root(),
    );

    let commit = create_changeset_no_parents(
        fb,
        &repo,
        root_manifest_future.map(Some).boxify(),
        vec![to_leaf(file_future), to_tree(manifest_dir_future)],
    );

    let bonsai_hg = commit.get_completed_changeset().compat().await.unwrap();
    let cs = &bonsai_hg.1;
    assert!(cs.manifestid() == root_mfid);
    assert!(cs.user() == author.as_bytes());
    assert!(cs.parents().get_nodes() == (None, None));
    let files: Vec<_> = cs.files().into();
    assert!(
        files == expected_files,
        format!("Got {:?}, expected {:?}", files, expected_files)
    );

    // And check the file blob is present
    let bytes = get_content(&ctx, &repo, filehash).await.unwrap();
    assert!(bytes.as_ref() == &b"blob"[..]);
}

#[fbinit::compat_test]
async fn create_two_changesets(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();
    let fake_file_path = RepoPath::file("dir/file").expect("Can't generate fake RepoPath");
    let fake_dir_path = RepoPath::dir("dir").expect("Can't generate fake RepoPath");
    let utf_author: String = "\u{041F}\u{0451}\u{0442}\u{0440} <peter@fb.com>".into();

    let (filehash, file_future) =
        upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path);

    let (dirhash, manifest_dir_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("file\0{}\n", filehash),
        &fake_dir_path,
    );

    let (roothash, root_manifest_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("dir\0{}t\n", dirhash),
        &RepoPath::root(),
    );

    let commit1 = create_changeset_no_parents(
        fb,
        &repo,
        root_manifest_future.map(Some).boxify(),
        vec![to_leaf(file_future), to_tree(manifest_dir_future)],
    );

    let fake_file_path_no_dir = RepoPath::file("file").expect("Can't generate fake RepoPath");
    let (filehash, file_future) =
        upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path_no_dir);
    let (roothash, root_manifest_future) = upload_manifest_one_parent(
        ctx.clone(),
        &repo,
        format!("file\0{}\n", filehash),
        &RepoPath::root(),
        roothash,
    );

    let commit2 = create_changeset_one_parent(
        fb,
        &repo,
        root_manifest_future.map(Some).boxify(),
        vec![to_leaf(file_future)],
        commit1.clone(),
    );

    let (commit1, commit2) = (commit1
        .get_completed_changeset()
        .join(commit2.get_completed_changeset()))
    .compat()
    .await
    .unwrap();

    let commit1 = &commit1.1;
    let commit2 = &commit2.1;
    assert!(commit2.manifestid() == roothash);
    assert!(commit2.user() == utf_author.as_bytes());
    let files: Vec<_> = commit2.files().into();
    let expected_files = vec![MPath::new("dir/file").unwrap(), MPath::new("file").unwrap()];
    assert!(
        files == expected_files,
        format!("Got {:?}, expected {:?}", files, expected_files)
    );

    assert!(commit1.parents().get_nodes() == (None, None));
    let commit1_id = Some(commit1.get_changeset_id().into_nodehash());
    let expected_parents = (commit1_id, None);
    assert!(commit2.parents().get_nodes() == expected_parents);
}

#[fbinit::compat_test]
async fn check_bonsai_creation(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();
    let fake_file_path = RepoPath::file("dir/file").expect("Can't generate fake RepoPath");
    let fake_dir_path = RepoPath::dir("dir").expect("Can't generate fake RepoPath");

    let (filehash, file_future) =
        upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path);

    let (dirhash, manifest_dir_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("file\0{}\n", filehash),
        &fake_dir_path,
    );

    let (_, root_manifest_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("dir\0{}t\n", dirhash),
        &RepoPath::root(),
    );

    let commit = create_changeset_no_parents(
        fb,
        &repo,
        root_manifest_future.map(Some).boxify(),
        vec![to_leaf(file_future), to_tree(manifest_dir_future)],
    );

    let commit = commit.get_completed_changeset().compat().await.unwrap();
    let commit = &commit.1;
    let bonsai_cs_id = repo
        .get_bonsai_from_hg(ctx.clone(), commit.get_changeset_id())
        .compat()
        .await
        .unwrap();
    assert!(bonsai_cs_id.is_some());
    let bonsai = bonsai_cs_id
        .unwrap()
        .load(&ctx, repo.blobstore())
        .await
        .unwrap();
    assert_eq!(
        bonsai
            .file_changes()
            .map(|fc| format!("{}", fc.0))
            .collect::<Vec<_>>(),
        vec![String::from("dir/file")]
    );
}

#[fbinit::compat_test]
async fn check_bonsai_creation_with_rename(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();
    let parent = {
        let fake_file_path = RepoPath::file("file").expect("Can't generate fake RepoPath");

        let (filehash, file_future) =
            upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path);

        let (_, root_manifest_future) = upload_manifest_no_parents(
            ctx.clone(),
            &repo,
            format!("file\0{}\n", filehash),
            &RepoPath::root(),
        );

        create_changeset_no_parents(
            fb,
            &repo,
            root_manifest_future.map(Some).boxify(),
            vec![to_leaf(file_future)],
        )
    };

    let child = {
        let fake_renamed_file_path =
            RepoPath::file("file_rename").expect("Can't generate fake RepoPath");

        let (filehash, file_future) = upload_file_no_parents(
            ctx.clone(),
            &repo,
            "\x01\ncopy: file\ncopyrev: c3127cdbf2eae0f09653f9237d85c8436425b246\x01\nblob",
            &fake_renamed_file_path,
        );

        let (_, root_manifest_future) = upload_manifest_no_parents(
            ctx.clone(),
            &repo,
            format!("file_rename\0{}\n", filehash),
            &RepoPath::root(),
        );

        create_changeset_one_parent(
            fb,
            &repo,
            root_manifest_future.map(Some).boxify(),
            vec![to_leaf(file_future)],
            parent.clone(),
        )
    };

    let parent_cs = parent.get_completed_changeset().compat().await.unwrap();
    let parent_cs = &parent_cs.1;
    let child_cs = child.get_completed_changeset().compat().await.unwrap();
    let child_cs = &child_cs.1;

    let parent_bonsai_cs_id = repo
        .get_bonsai_from_hg(ctx.clone(), parent_cs.get_changeset_id())
        .compat()
        .await
        .unwrap()
        .unwrap();

    let bonsai_cs_id = repo
        .get_bonsai_from_hg(ctx.clone(), child_cs.get_changeset_id())
        .compat()
        .await
        .unwrap();
    let bonsai = bonsai_cs_id
        .unwrap()
        .load(&ctx, repo.blobstore())
        .await
        .unwrap();
    let fc = bonsai.file_changes().collect::<BTreeMap<_, _>>();
    let file = MPath::new("file").unwrap();
    assert!(!fc[&file].is_some());
    let file_rename = MPath::new("file_rename").unwrap();
    assert!(fc[&file_rename].is_some());
    assert_eq!(
        fc[&file_rename].unwrap().copy_from(),
        Some(&(file, parent_bonsai_cs_id))
    );
}

#[fbinit::compat_test]
async fn create_bad_changeset(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();
    let dirhash = string_to_nodehash("c2d60b35a8e7e034042a9467783bbdac88a0d219");

    let (_, root_manifest_future) = upload_manifest_no_parents(
        ctx,
        &repo,
        format!("dir\0{}t\n", dirhash),
        &RepoPath::root(),
    );

    let commit =
        create_changeset_no_parents(fb, &repo, root_manifest_future.map(Some).boxify(), vec![]);

    commit
        .get_completed_changeset()
        .compat()
        .await
        .expect_err("This should fail");
}

#[fbinit::compat_test]
async fn upload_entries_finalize_success(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();

    let fake_file_path = RepoPath::file("file").expect("Can't generate fake RepoPath");

    let (filehash, file_future) =
        upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path);

    let (roothash, root_manifest_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("file\0{}\n", filehash),
        &RepoPath::root(),
    );

    let (fnid, _) = file_future.compat().await.unwrap();
    let (root_mfid, _) = root_manifest_future.compat().await.unwrap();

    let entries = UploadEntries::new(repo.get_blobstore(), ScubaSampleBuilder::with_discard());

    (entries.process_root_manifest(&ctx, root_mfid))
        .await
        .unwrap();

    (entries.process_one_entry(&ctx, Entry::Leaf(fnid), fake_file_path))
        .await
        .unwrap();

    (entries.finalize(ctx.clone(), roothash, vec![]))
        .compat()
        .await
        .unwrap();
}

#[fbinit::compat_test]
async fn upload_entries_finalize_fail(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();

    let entries = UploadEntries::new(repo.get_blobstore(), ScubaSampleBuilder::with_discard());

    let dirhash = string_to_nodehash("c2d60b35a8e7e034042a9467783bbdac88a0d219");
    let (_, root_manifest_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("dir\0{}t\n", dirhash),
        &RepoPath::root(),
    );
    let (root_mfid, _) = (root_manifest_future).compat().await.unwrap();

    (entries.process_root_manifest(&ctx, root_mfid))
        .await
        .unwrap();

    let res = (entries.finalize(ctx.clone(), root_mfid, vec![]))
        .compat()
        .await;

    assert!(res.is_err());
}

#[fbinit::compat_test]
async fn test_compute_changed_files_no_parents(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = many_files_dirs::getrepo(fb).await;
    let nodehash = string_to_nodehash("051946ed218061e925fb120dac02634f9ad40ae2");
    let expected = vec![
        MPath::new(b"1").unwrap(),
        MPath::new(b"2").unwrap(),
        MPath::new(b"dir1").unwrap(),
        MPath::new(b"dir2/file_1_in_dir2").unwrap(),
    ];

    let cs = HgChangesetId::new(nodehash)
        .load(&ctx, repo.blobstore())
        .await
        .unwrap();

    let diff = (compute_changed_files(ctx.clone(), repo.clone(), cs.manifestid(), None, None))
        .compat()
        .await
        .unwrap();
    assert!(
        diff == expected,
        "Got {:?}, expected {:?}\n",
        diff,
        expected,
    );
}

#[fbinit::compat_test]
async fn test_compute_changed_files_one_parent(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    // Note that this is a commit and its parent commit, so you can use:
    // hg log -T"{node}\n{files % '    MPath::new(b\"{file}\").unwrap(),\\n'}\\n" -r $HASH
    // to see how Mercurial would compute the files list and confirm that it's the same
    let repo = many_files_dirs::getrepo(fb).await;
    let nodehash = string_to_nodehash("051946ed218061e925fb120dac02634f9ad40ae2");
    let parenthash = string_to_nodehash("d261bc7900818dea7c86935b3fb17a33b2e3a6b4");
    let expected = vec![
        MPath::new(b"dir1").unwrap(),
        MPath::new(b"dir1/file_1_in_dir1").unwrap(),
        MPath::new(b"dir1/file_2_in_dir1").unwrap(),
        MPath::new(b"dir1/subdir1/file_1").unwrap(),
        MPath::new(b"dir1/subdir1/subsubdir1/file_1").unwrap(),
        MPath::new(b"dir1/subdir1/subsubdir2/file_1").unwrap(),
        MPath::new(b"dir1/subdir1/subsubdir2/file_2").unwrap(),
    ];

    let cs = (HgChangesetId::new(nodehash).load(&ctx, repo.blobstore()))
        .await
        .unwrap();

    let parent_cs = (HgChangesetId::new(parenthash).load(&ctx, repo.blobstore()))
        .await
        .unwrap();

    let diff = (compute_changed_files(
        ctx.clone(),
        repo.clone(),
        cs.manifestid(),
        Some(parent_cs.manifestid()),
        None,
    ))
    .compat()
    .await
    .unwrap();
    assert!(
        diff == expected,
        "Got {:?}, expected {:?}\n",
        diff,
        expected,
    );
}

fn make_bonsai_changeset(
    p0: Option<ChangesetId>,
    p1: Option<ChangesetId>,
    changes: Vec<(&'static str, Option<FileChange>)>,
) -> BonsaiChangeset {
    BonsaiChangesetMut {
        parents: p0.into_iter().chain(p1).collect(),
        author: "aslpavel".to_owned(),
        author_date: DateTime::from_timestamp(1528298184, 0).unwrap(),
        committer: None,
        committer_date: None,
        message: "[mononoke] awesome message".to_owned(),
        extra: BTreeMap::new(),
        file_changes: changes
            .into_iter()
            .map(|(path, change)| (MPath::new(path).unwrap(), change))
            .collect(),
    }
    .freeze()
    .unwrap()
}

fn make_file_change<'a>(
    ctx: &'a CoreContext,
    content: impl AsRef<[u8]>,
    repo: &'a BlobRepo,
) -> impl Future<Item = FileChange, Error = Error> + Send + 'a {
    let content = content.as_ref();
    let content_size = content.len() as u64;
    FileContents::new_bytes(Bytes::copy_from_slice(content))
        .into_blob()
        .store(ctx, repo.blobstore())
        .compat()
        .map(move |content_id| FileChange::new(content_id, FileType::Regular, content_size, None))
}

fn entry_nodehash(e: &Entry<HgManifestId, (FileType, HgFileNodeId)>) -> HgNodeHash {
    match e {
        Entry::Leaf((_, id)) => id.into_nodehash(),
        Entry::Tree(id) => id.into_nodehash(),
    }
}

async fn entry_content(
    ctx: &CoreContext,
    repo: &BlobRepo,
    e: &Entry<HgManifestId, (FileType, HgFileNodeId)>,
) -> Result<Bytes, Error> {
    let ret = match e {
        Entry::Leaf((_, id)) => {
            let envelope = id.load(ctx, repo.blobstore()).await?;
            filestore::fetch_concat(&repo.get_blobstore(), ctx, envelope.content_id()).await?
        }
        Entry::Tree(..) => {
            return Err(Error::msg("entry_content was called on a Tree"));
        }
    };

    Ok(ret)
}

async fn entry_parents(
    ctx: &CoreContext,
    repo: &BlobRepo,
    e: &Entry<HgManifestId, (FileType, HgFileNodeId)>,
) -> Result<HgParents, Error> {
    let ret = match e {
        Entry::Leaf((_, id)) => {
            let envelope = id.load(ctx, repo.blobstore()).await?;
            envelope.hg_parents()
        }
        Entry::Tree(id) => {
            let manifest = id.load(ctx, repo.blobstore()).await?;
            manifest.hg_parents()
        }
    };

    Ok(ret)
}

#[fbinit::compat_test]
async fn test_get_manifest_from_bonsai(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = merge_uneven::getrepo(fb).await;

    let get_entries = {
        cloned!(ctx, repo);
        move |ms_hash: HgManifestId| -> BoxFuture<HashMap<String, Entry<HgManifestId, (FileType, HgFileNodeId)>>, Error> {
                    cloned!(ctx, repo);
                    async move {
                        ms_hash.load(&ctx, repo.blobstore()).await
                    }
                    .boxed()
                    .compat()
                    .from_err()
                    .map(|ms| {
                        Manifest::list(&ms)
                            .map(|(name, entry)| {
                                (String::from_utf8(Vec::from(name.as_ref())).unwrap(), entry)
                            })
                            .collect::<HashMap<_, _>>()
                    })
                    .boxify()
            }
    };

    // #CONTENT
    // 1: 1
    // 2: 2
    // 3: 3
    // 4: 4
    // 5: 5
    // base: branch1
    // branch: 4
    let ms1 = HgChangesetId::new(string_to_nodehash(
        "264f01429683b3dd8042cb3979e8bf37007118bc",
    ))
    .load(&ctx, repo.blobstore())
    .await
    .unwrap()
    .manifestid();

    // #CONTENT
    // base: base
    // branch: 4
    let ms2 = HgChangesetId::new(string_to_nodehash(
        "16839021e338500b3cf7c9b871c8a07351697d68",
    ))
    .load(&ctx, repo.blobstore())
    .await
    .unwrap()
    .manifestid();

    // fails with conflict
    {
        let ms_hash = (get_manifest_from_bonsai(
            &repo,
            ctx.clone(),
            make_bonsai_changeset(None, None, vec![]),
            vec![ms1, ms2],
        ))
        .compat()
        .await;
        assert!(
            ms_hash
                .expect_err("should have failed")
                .to_string()
                .contains("conflict")
        );
    }

    // resolves same content different parents for `branch` file
    {
        let ms_hash = (get_manifest_from_bonsai(
            &repo,
            ctx.clone(),
            make_bonsai_changeset(None, None, vec![("base", None)]),
            vec![ms1, ms2],
        ))
        .compat()
        .await
        .expect("merge should have succeeded");
        let entries = (get_entries(ms_hash)).compat().await.unwrap();

        assert!(entries.get("1").is_some());
        assert!(entries.get("2").is_some());
        assert!(entries.get("3").is_some());
        assert!(entries.get("4").is_some());
        assert!(entries.get("5").is_some());
        assert!(entries.get("base").is_none());

        // check trivial merge parents
        let (ms1_entries, ms2_entries) = (get_entries(ms1).join(get_entries(ms2)))
            .compat()
            .await
            .unwrap();
        let mut br_expected_parents = HashSet::new();
        br_expected_parents.insert(entry_nodehash(ms1_entries.get("branch").unwrap()));
        br_expected_parents.insert(entry_nodehash(ms2_entries.get("branch").unwrap()));

        let br = entries.get("branch").expect("trivial merge should succeed");
        let br_parents = entry_parents(&ctx, &repo, br)
            .await
            .unwrap()
            .into_iter()
            .collect::<HashSet<_>>();
        assert_eq!(br_parents, br_expected_parents);
    }

    // add file
    {
        let content_expected = &b"some awesome content"[..];
        let fc = (make_file_change(&ctx, content_expected, &repo))
            .compat()
            .await
            .unwrap();
        let bcs = make_bonsai_changeset(None, None, vec![("base", None), ("new", Some(fc))]);
        let ms_hash = (get_manifest_from_bonsai(&repo, ctx.clone(), bcs, vec![ms1, ms2]))
            .compat()
            .await
            .expect("adding new file should not produce coflict");
        let entries = (get_entries(ms_hash)).compat().await.unwrap();
        let new = entries.get("new").expect("new file should be in entries");
        let bytes = entry_content(&ctx, &repo, new).await.unwrap();
        assert_eq!(bytes.as_ref(), content_expected.as_ref());

        let new_parents = (entry_parents(&ctx, &repo, new)).await.unwrap();
        assert_eq!(new_parents, HgParents::None);
    }
}

#[fbinit::compat_test]
async fn test_case_conflict_in_manifest(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = many_files_dirs::getrepo(fb).await;
    let hg_cs = HgChangesetId::new(string_to_nodehash(
        "2f866e7e549760934e31bf0420a873f65100ad63",
    ));

    let mf = hg_cs
        .load(&ctx, repo.blobstore())
        .await
        .unwrap()
        .manifestid();

    let bonsai_parent = (repo.get_bonsai_from_hg(ctx.clone(), hg_cs))
        .compat()
        .await
        .unwrap()
        .unwrap();

    for (path, expect_conflict) in &[
        ("dir1/file_1_in_dir1", false),
        ("dir1/file_1_IN_dir1", true),
        ("DiR1/file_1_in_dir1", true),
        ("dir1/other_dir/file", false),
    ] {
        let bcs_id = create_commit(
            ctx.clone(),
            repo.clone(),
            vec![bonsai_parent],
            store_files(&ctx, btreemap! {*path => Some("caseconflicttest")}, &repo).await,
        )
        .await;

        let child_hg_cs = (repo.get_hg_from_bonsai_changeset(ctx.clone(), bcs_id.clone()))
            .compat()
            .await
            .unwrap();
        let child_mf = child_hg_cs
            .load(&ctx, repo.blobstore())
            .await
            .unwrap()
            .manifestid();

        let conflicts = check_case_conflicts(&ctx, &repo, child_mf, Some(mf)).await;
        if *expect_conflict {
            assert_matches!(
                conflicts
                    .as_ref()
                    .map_err(|e| e.downcast_ref::<ErrorKind>()),
                Err(Some(ErrorKind::ExternalCaseConflict(..)))
            );
        } else {
            assert_matches!(conflicts, Ok(()));
        }
    }
}

#[fbinit::compat_test]
async fn test_case_conflict_two_changeset(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();

    let fake_file_path_1 = RepoPath::file("file").expect("Can't generate fake RepoPath");
    let (filehash_1, file_future_1) =
        upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path_1);

    let (_roothash, root_manifest_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("file\0{}\n", filehash_1),
        &RepoPath::root(),
    );

    let commit1 = create_changeset_no_parents(
        fb,
        &repo,
        root_manifest_future.map(Some).boxify(),
        vec![to_leaf(file_future_1)],
    );

    let commit2 = {
        let fake_file_path_2 = RepoPath::file("FILE").expect("Can't generate fake RepoPath");
        let (filehash_2, file_future_2) =
            upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path_2);
        let (_roothash, root_manifest_future) = upload_manifest_no_parents(
            ctx.clone(),
            &repo,
            format!("file\0{}\nFILE\0{}\n", filehash_1, filehash_2),
            &RepoPath::root(),
        );

        create_changeset_one_parent(
            fb,
            &repo,
            root_manifest_future.map(Some).boxify(),
            vec![to_leaf(file_future_2)],
            commit1.clone(),
        )
    };

    assert!(
        commit1
            .get_completed_changeset()
            .join(commit2.get_completed_changeset())
            .compat()
            .await
            .is_err()
    );
}

#[fbinit::compat_test]
async fn test_case_conflict_inside_one_changeset(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();
    let fake_file_path_1 = RepoPath::file("file").expect("Can't generate fake RepoPath");
    let (filehash_1, file_future_1) =
        upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path_1);

    let fake_file_path_1 = RepoPath::file("FILE").expect("Can't generate fake RepoPath");
    let (filehash_2, file_future_2) =
        upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path_1);

    let (_roothash, root_manifest_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("file\0{}\nFILE\0{}", filehash_1, filehash_2),
        &RepoPath::root(),
    );

    let commit1 = create_changeset_no_parents(
        fb,
        &repo,
        root_manifest_future.map(Some).boxify(),
        vec![to_leaf(file_future_1), to_leaf(file_future_2)],
    );

    assert!((commit1.get_completed_changeset()).compat().await.is_err());
}

#[fbinit::compat_test]
async fn test_no_case_conflict_removal(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();

    let fake_file_path_1 = RepoPath::file("file").expect("Can't generate fake RepoPath");
    let (filehash_1, file_future_1) =
        upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path_1);

    let (_roothash, root_manifest_future) = upload_manifest_no_parents(
        ctx.clone(),
        &repo,
        format!("file\0{}\n", filehash_1),
        &RepoPath::root(),
    );

    let commit1 = create_changeset_no_parents(
        fb,
        &repo,
        root_manifest_future.map(Some).boxify(),
        vec![to_leaf(file_future_1)],
    );

    let commit2 = {
        let fake_file_path_2 = RepoPath::file("FILE").expect("Can't generate fake RepoPath");
        let (filehash_2, file_future_2) =
            upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path_2);
        let (_roothash, root_manifest_future) = upload_manifest_no_parents(
            ctx.clone(),
            &repo,
            format!("FILE\0{}\n", filehash_2),
            &RepoPath::root(),
        );

        create_changeset_one_parent(
            fb,
            &repo,
            root_manifest_future.map(Some).boxify(),
            vec![to_leaf(file_future_2)],
            commit1.clone(),
        )
    };

    assert!(
        (commit1
            .get_completed_changeset()
            .join(commit2.get_completed_changeset()))
        .compat()
        .await
        .is_ok()
    );
}

#[fbinit::compat_test]
async fn test_no_case_conflict_removal_dir(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = get_empty_repo();

    let commit1 = {
        let fake_file_path = RepoPath::file("file").expect("Can't generate fake RepoPath");
        let fake_dir_path = RepoPath::file("dir").expect("Can't generate fake RepoPath");
        let (filehash, file_future) =
            upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path);

        let (dirhash_1, manifest_dir_future) = upload_manifest_no_parents(
            ctx.clone(),
            &repo,
            format!("file\0{}\n", filehash),
            &fake_dir_path,
        );

        let (_roothash, root_manifest_future) = upload_manifest_no_parents(
            ctx.clone(),
            &repo,
            format!("dir\0{}t\n", dirhash_1),
            &RepoPath::root(),
        );

        create_changeset_no_parents(
            fb,
            &repo,
            root_manifest_future.map(Some).boxify(),
            vec![to_leaf(file_future), to_tree(manifest_dir_future)],
        )
    };

    let commit2 = {
        let fake_file_path = RepoPath::file("file").expect("Can't generate fake RepoPath");
        let fake_dir_path = RepoPath::file("DIR").expect("Can't generate fake RepoPath");
        let (filehash, file_future) =
            upload_file_no_parents(ctx.clone(), &repo, "blob", &fake_file_path);

        let (dirhash_1, manifest_dir_future) = upload_manifest_no_parents(
            ctx.clone(),
            &repo,
            format!("file\0{}\n", filehash),
            &fake_dir_path,
        );

        let (_roothash, root_manifest_future) = upload_manifest_no_parents(
            ctx.clone(),
            &repo,
            format!("DIR\0{}t\n", dirhash_1),
            &RepoPath::root(),
        );

        create_changeset_one_parent(
            fb,
            &repo,
            root_manifest_future.map(Some).boxify(),
            vec![to_leaf(file_future), to_tree(manifest_dir_future)],
            commit1.clone(),
        )
    };

    assert!(
        (commit1
            .get_completed_changeset()
            .join(commit2.get_completed_changeset()))
        .compat()
        .await
        .is_ok()
    );
}

#[fbinit::compat_test]
async fn test_hg_commit_generation_simple(fb: FacebookInit) {
    let repo = fixtures::linear::getrepo(fb).await;
    let bcs = create_bonsai_changeset(vec![]);

    let bcs_id = bcs.get_changeset_id();
    let ctx = CoreContext::test_mock(fb);
    blobrepo::save_bonsai_changesets(vec![bcs], ctx.clone(), repo.clone())
        .compat()
        .await
        .unwrap();
    let hg_cs_id = repo
        .get_hg_from_bonsai_changeset(ctx.clone(), bcs_id)
        .compat()
        .await
        .unwrap();

    assert_eq!(
        hg_cs_id,
        HgChangesetId::new(string_to_nodehash(
            "5c31d1196c64c93cb5bcf8bca3a24860f103d69f"
        ))
    );
    // make sure bonsai hg mapping is updated
    let map_bcs_id = repo
        .get_bonsai_from_hg(ctx, hg_cs_id)
        .compat()
        .await
        .unwrap();
    assert_eq!(map_bcs_id, Some(bcs_id));
}

#[fbinit::compat_test]
async fn test_hg_commit_generation_stack(fb: FacebookInit) {
    let repo = fixtures::linear::getrepo(fb).await;
    let mut changesets = vec![];
    let bcs = create_bonsai_changeset(vec![]);

    let mut prev_bcs_id = bcs.get_changeset_id();
    changesets.push(bcs.clone());

    // Create a large stack to make sure we don't have stackoverflow problems
    let stack_size = 10000;
    for _ in 1..stack_size {
        let new_bcs = create_bonsai_changeset(vec![prev_bcs_id]);
        prev_bcs_id = new_bcs.get_changeset_id();
        changesets.push(new_bcs);
    }

    let top_of_stack = changesets.last().unwrap().clone().get_changeset_id();
    let ctx = CoreContext::test_mock(fb);
    blobrepo::save_bonsai_changesets(changesets, ctx.clone(), repo.clone())
        .compat()
        .await
        .unwrap();

    let hg_cs_id = repo
        .get_hg_from_bonsai_changeset(ctx, top_of_stack)
        .compat()
        .await
        .unwrap();
    assert_eq!(
        hg_cs_id,
        HgChangesetId::new(string_to_nodehash(
            "b15a980d805db1646422dbf02016aa8a9f8aacd3",
        ))
    );
}

#[fbinit::compat_test]
async fn test_hg_commit_generation_one_after_another(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = fixtures::linear::getrepo(fb).await;

    let first_bcs = create_bonsai_changeset(vec![]);
    let first_bcs_id = first_bcs.get_changeset_id();

    let second_bcs = create_bonsai_changeset(vec![first_bcs_id]);
    let second_bcs_id = second_bcs.get_changeset_id();
    blobrepo::save_bonsai_changesets(vec![first_bcs, second_bcs], ctx.clone(), repo.clone())
        .compat()
        .await
        .unwrap();

    let hg_cs_id = repo
        .get_hg_from_bonsai_changeset(ctx.clone(), first_bcs_id)
        .compat()
        .await
        .unwrap();
    assert_eq!(
        hg_cs_id,
        HgChangesetId::new(string_to_nodehash(
            "5c31d1196c64c93cb5bcf8bca3a24860f103d69f",
        ))
    );

    let hg_cs_id = repo
        .get_hg_from_bonsai_changeset(ctx, second_bcs_id)
        .compat()
        .await
        .unwrap();
    assert_eq!(
        hg_cs_id,
        HgChangesetId::new(string_to_nodehash(
            "09e9a31873e07ad483aa64e4dfd2cc705de40276",
        ))
    );
}

#[fbinit::compat_test]
async fn test_hg_commit_generation_diamond(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = fixtures::linear::getrepo(fb).await;

    let last_bcs_id = fixtures::save_diamond_commits(ctx.clone(), repo.clone(), vec![])
        .await
        .unwrap();

    let hg_cs_id = repo
        .get_hg_from_bonsai_changeset(ctx.clone(), last_bcs_id)
        .compat()
        .await
        .unwrap();
    assert_eq!(
        hg_cs_id,
        HgChangesetId::new(string_to_nodehash(
            "5d69478d73e67e5270550e44f2acfd93f456d74a",
        ))
    );
}

#[fbinit::compat_test]
async fn test_hg_commit_generation_many_diamond(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = fixtures::many_diamonds::getrepo(fb).await;
    let book = bookmarks::BookmarkName::new("master").unwrap();
    let bcs_id = repo
        .get_bonsai_bookmark(ctx.clone(), &book)
        .compat()
        .await
        .unwrap()
        .unwrap();

    let hg_cs_id = repo
        .get_hg_from_bonsai_changeset(ctx, bcs_id)
        .compat()
        .await
        .unwrap();
    assert_eq!(
        hg_cs_id,
        HgChangesetId::new(string_to_nodehash(
            "6b43556e77b7312cabd16ac5f0a85cd920d95272",
        ))
    );
}

#[fbinit::compat_test]
async fn test_hg_commit_generation_uneven_branch(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    let repo = blobrepo_factory::new_memblob_empty(None).expect("cannot create empty repo");

    let root_bcs = fixtures::create_bonsai_changeset(vec![]);

    let large_branch_1 = fixtures::create_bonsai_changeset(vec![root_bcs.get_changeset_id()]);
    let large_branch_2 = fixtures::create_bonsai_changeset(vec![large_branch_1.get_changeset_id()]);

    let short_branch = fixtures::create_bonsai_changeset(vec![root_bcs.get_changeset_id()]);

    let merge = fixtures::create_bonsai_changeset(vec![
        short_branch.get_changeset_id(),
        large_branch_2.get_changeset_id(),
    ]);

    blobrepo::save_bonsai_changesets(
        vec![
            root_bcs,
            large_branch_1,
            large_branch_2,
            short_branch,
            merge.clone(),
        ],
        ctx.clone(),
        repo.clone(),
    )
    .compat()
    .await
    .unwrap();

    let hg_cs_id = repo
        .get_hg_from_bonsai_changeset(ctx.clone(), merge.get_changeset_id())
        .compat()
        .await
        .unwrap();
    assert_eq!(
        hg_cs_id,
        HgChangesetId::new(string_to_nodehash(
            "62b3de4cbd1bc4bf8422c6588234c28842476d3b",
        ))
    );
}

#[cfg(fbcode_build)]
#[fbinit::compat_test]
async fn save_reproducibility_under_load(fb: FacebookInit) -> Result<(), Error> {
    use benchmark_lib::{new_benchmark_repo, DelaySettings, GenManifest};
    use rand::SeedableRng;
    use rand_distr::Normal;
    use rand_xorshift::XorShiftRng;

    let ctx = CoreContext::test_mock(fb);
    let delay_settings = DelaySettings {
        blobstore_put_dist: Normal::new(0.01, 0.005).expect("Normal::new failed"),
        blobstore_get_dist: Normal::new(0.005, 0.0025).expect("Normal::new failed"),
        db_put_dist: Normal::new(0.002, 0.001).expect("Normal::new failed"),
        db_get_dist: Normal::new(0.002, 0.001).expect("Normal::new failed"),
    };
    cmdlib::args::init_cachelib(fb, &Default::default(), None);
    let repo = new_benchmark_repo(fb, delay_settings)?;

    let mut rng = XorShiftRng::seed_from_u64(1);
    let mut gen = GenManifest::new();
    let settings = Default::default();

    let test = gen
        .gen_stack(
            ctx.clone(),
            repo.clone(),
            &mut rng,
            &settings,
            None,
            std::iter::repeat(16).take(50),
        )
        .boxed()
        .compat()
        .and_then(move |csid| repo.get_hg_from_bonsai_changeset(ctx, csid));

    assert_eq!(
        test.compat().await?,
        "e9b73f926c993c5232139d4eefa6f77fa8c41279".parse()?,
    );

    Ok(())
}

#[fbinit::compat_test]
async fn test_filenode_lookup(fb: FacebookInit) -> Result<(), Error> {
    let ctx = CoreContext::test_mock(fb);

    let memblob = Memblob::default();
    let blobstore = Arc::new(TracingBlobstore::new(memblob));

    let repo = blobrepo_factory::new_memblob_empty(Some(blobstore.clone()))?;

    let p1 = None;
    let p2 = None;

    let content_blob = FileContents::new_bytes(
        File::new(b"myblob".to_vec(), p1, p2)
            .file_contents()
            .into_bytes(),
    )
    .into_blob();
    let content_id = *content_blob.id();
    let content_len = content_blob.len() as u64;
    content_blob.store(&ctx, repo.blobstore()).await?;

    let path1 = RepoPath::file("path/1")?;
    let path2 = RepoPath::file("path/2")?;
    let path3 = RepoPath::file("path/3")?;

    let content_key = format!("repo0000.content.blake2.{}", content_id.to_hex());

    let cbmeta = ContentBlobMeta {
        id: content_id,
        size: content_len,
        copy_from: None,
    };

    let cbmeta_copy = ContentBlobMeta {
        id: content_id,
        size: content_len,
        copy_from: Some((to_mpath(path3.clone())?, ONES_FNID)),
    };

    // Clear our blobstore first.
    let _ = blobstore.tracing_gets();

    // First, upload. We expect 3 calls here:
    // - Filenode lookup: this will miss.
    // - File lookup (to compute metadata): this will hit.
    // - File lookup (to hash the contents): this will hit.

    let upload = UploadHgFileEntry {
        upload_node_id: UploadHgNodeHash::Generate,
        contents: UploadHgFileContents::ContentUploaded(cbmeta.clone()),
        p1,
        p2,
        path: to_mpath(path1.clone())?,
    };
    let (_, future) = upload.upload(ctx.clone(), repo.get_blobstore().boxed())?;
    future.compat().await?;

    let gets = blobstore.tracing_gets();
    assert_eq!(gets.len(), 3);
    assert!(gets[0].contains("filenode_lookup"));
    assert_eq!(gets[1], content_key);
    assert_eq!(gets[2], content_key);

    // Now, upload the content again. This time, we expect one call to the alias, and one call to
    // fetch the metadata (this is obviously a little inefficient if we need both, but the latter
    // call can now be reduced to peeking at the file contents).

    let upload = UploadHgFileEntry {
        upload_node_id: UploadHgNodeHash::Generate,
        contents: UploadHgFileContents::ContentUploaded(cbmeta.clone()),
        p1,
        p2,
        path: to_mpath(path2.clone())?,
    };
    let (_, future) = upload.upload(ctx.clone(), repo.get_blobstore().boxed())?;
    future.compat().await?;

    let gets = blobstore.tracing_gets();
    assert_eq!(gets.len(), 2);
    assert!(gets[0].contains("filenode_lookup"));
    assert_eq!(gets[1], content_key);

    // Finally, upload with different copy metadata. Reusing the filenode should not be possible,
    // so this should make 3 calls again.
    let upload = UploadHgFileEntry {
        upload_node_id: UploadHgNodeHash::Generate,
        contents: UploadHgFileContents::ContentUploaded(cbmeta_copy.clone()),
        p1,
        p2,
        path: to_mpath(path2.clone())?,
    };
    let (_, future) = upload.upload(ctx.clone(), repo.get_blobstore().boxed())?;
    future.compat().await?;

    let gets = blobstore.tracing_gets();
    assert_eq!(gets.len(), 3);
    assert!(gets[0].contains("filenode_lookup"));
    assert_eq!(gets[1], content_key);
    assert_eq!(gets[2], content_key);

    Ok(())
}

#[fbinit::compat_test]
async fn test_content_uploaded_filenode_id(fb: FacebookInit) -> Result<(), Error> {
    let ctx = CoreContext::test_mock(fb);

    let repo = blobrepo_factory::new_memblob_empty(None)?;

    let p1 = None;
    let p2 = None;

    let content_blob = FileContents::new_bytes(
        File::new(b"myblob".to_vec(), p1, p2)
            .file_contents()
            .into_bytes(),
    )
    .into_blob();
    let content_id = *content_blob.id();
    let content_len = content_blob.len() as u64;
    content_blob.store(&ctx, repo.blobstore()).await?;

    let path1 = RepoPath::file("path/1")?;
    let path2 = RepoPath::file("path/2")?;

    let cbmeta = ContentBlobMeta {
        id: content_id,
        size: content_len,
        copy_from: Some((to_mpath(path2.clone())?, ONES_FNID)),
    };

    let upload = UploadHgFileEntry {
        upload_node_id: UploadHgNodeHash::Checked(
            "47f917b28e191c4bb0de8927e716e1b976ec3ad0".parse()?,
        ),
        contents: UploadHgFileContents::ContentUploaded(cbmeta.clone()),
        p1,
        p2,
        path: to_mpath(path1.clone())?,
    };
    let (_, future) = upload.upload(ctx.clone(), repo.get_blobstore().boxed())?;
    future.compat().await?;

    Ok(())
}

struct TestHelper {
    ctx: CoreContext,
    repo: BlobRepo,
}

impl TestHelper {
    fn new(fb: FacebookInit) -> Result<Self, Error> {
        let ctx = CoreContext::test_mock(fb);
        let repo = blobrepo_factory::new_memblob_empty(None)?;
        Ok(Self { ctx, repo })
    }

    fn new_commit(&self) -> CreateCommitContext<'_> {
        CreateCommitContext::new_root(&self.ctx, &self.repo)
    }

    async fn lookup_changeset(&self, cs_id: ChangesetId) -> Result<HgBlobChangeset, Error> {
        let hg_cs_id = self
            .repo
            .get_hg_from_bonsai_changeset(self.ctx.clone(), cs_id)
            .compat()
            .await?;

        let hg_cs = hg_cs_id.load(&self.ctx, self.repo.blobstore()).await?;

        Ok(hg_cs)
    }

    async fn root_manifest(&self, cs_id: ChangesetId) -> Result<HgBlobManifest, Error> {
        let hg_cs = self.lookup_changeset(cs_id).await?;

        let manifest = hg_cs
            .manifestid()
            .load(&self.ctx, self.repo.blobstore())
            .await?;

        Ok(manifest)
    }

    async fn lookup_entry(
        &self,
        cs_id: ChangesetId,
        path: &str,
    ) -> Result<Entry<HgManifestId, (FileType, HgFileNodeId)>, Error> {
        let path = MPath::new(path)?;

        let hg_cs = self.lookup_changeset(cs_id).await?;

        let err = Error::msg(format!("Missing entry: {}", path));

        let entry = hg_cs
            .manifestid()
            .find_entry(self.ctx.clone(), self.repo.get_blobstore(), Some(path))
            .await?
            .ok_or(err)?;

        Ok(entry)
    }

    async fn lookup_manifest(
        &self,
        cs_id: ChangesetId,
        path: &str,
    ) -> Result<HgBlobManifest, Error> {
        let id = self
            .lookup_entry(cs_id, path)
            .await?
            .into_tree()
            .ok_or_else(|| Error::msg(format!("Not a manifest: {}", path)))?;

        let manifest = id.load(&self.ctx, self.repo.blobstore()).await?;

        Ok(manifest)
    }

    async fn lookup_filenode(
        &self,
        cs_id: ChangesetId,
        path: &str,
    ) -> Result<HgFileEnvelope, Error> {
        let (_, filenode) = self
            .lookup_entry(cs_id, path)
            .await?
            .into_leaf()
            .ok_or_else(|| Error::msg(format!("Not a filenode: {}", path)))?;

        let envelope = filenode.load(&self.ctx, self.repo.blobstore()).await?;

        Ok(envelope)
    }
}

mod octopus_merges {
    use super::*;

    #[fbinit::compat_test]
    async fn test_basic(fb: FacebookInit) -> Result<(), Error> {
        let ctx = CoreContext::test_mock(fb);
        let repo = blobrepo_factory::new_memblob_empty(None)?;

        let p1 = CreateCommitContext::new_root(&ctx, &repo)
            .add_file("foo", "foo")
            .commit()
            .await?;

        let p2 = CreateCommitContext::new_root(&ctx, &repo)
            .add_file("bar", "bar")
            .commit()
            .await?;

        let p3 = CreateCommitContext::new_root(&ctx, &repo)
            .add_file("qux", "qux")
            .commit()
            .await?;

        let commit = CreateCommitContext::new(&ctx, &repo, vec![p1, p2, p3])
            .commit()
            .await?;

        let hg_cs_id = repo
            .get_hg_from_bonsai_changeset(ctx.clone(), commit)
            .compat()
            .await?;

        let hg_cs = hg_cs_id.load(&ctx, repo.blobstore()).await?;

        let hg_manifest = hg_cs.manifestid().load(&ctx, repo.blobstore()).await?;

        // Do we get the same files?
        let files = Manifest::list(&hg_manifest);
        assert_eq!(files.count(), 3);

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_basic_filenode_parents(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper.new_commit().add_file("foo", "foo").commit().await?;
        let p2 = helper.new_commit().add_file("bar", "bar").commit().await?;
        let p3 = helper.new_commit().add_file("qux", "qux").commit().await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .add_file("foo", "foo2")
            .add_file("bar", "bar2")
            .add_file("qux", "qux2")
            .commit()
            .await?;

        let foo = helper.lookup_filenode(commit, "foo").await?;
        let bar = helper.lookup_filenode(commit, "bar").await?;
        let qux = helper.lookup_filenode(commit, "qux").await?;

        // We expect the parents for foo and bar to be present, but qux should have it parent
        // dropped, because it's out of range for Mercurial.
        assert_eq!(
            foo.parents(),
            (
                Some(helper.lookup_filenode(p1, "foo").await?.node_id()),
                None
            )
        );

        assert_eq!(
            bar.parents(),
            (
                Some(helper.lookup_filenode(p2, "bar").await?.node_id()),
                None
            )
        );

        assert_eq!(qux.parents(), (None, None));

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_many_filenode_parents(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper.new_commit().add_file("foo", "foo").commit().await?;
        let p2 = helper.new_commit().add_file("foo", "bar").commit().await?;
        let p3 = helper.new_commit().add_file("foo", "qux").commit().await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .add_file("foo", "foo2")
            .commit()
            .await?;

        let foo = helper.lookup_filenode(commit, "foo").await?;

        assert_eq!(
            foo.parents(),
            (
                Some(helper.lookup_filenode(p1, "foo").await?.node_id()),
                Some(helper.lookup_filenode(p2, "foo").await?.node_id())
            )
        );

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_mixed_filenode_parents(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper.new_commit().commit().await?;
        let p2 = helper.new_commit().add_file("foo", "foo").commit().await?;
        let p3 = helper.new_commit().add_file("foo", "bar").commit().await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .add_file("foo", "foo2")
            .commit()
            .await?;

        let foo = helper.lookup_filenode(commit, "foo").await?;

        assert_eq!(
            foo.parents(),
            (
                Some(helper.lookup_filenode(p2, "foo").await?.node_id()),
                None
            )
        );

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_strip_copy_from(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper.new_commit().commit().await?;
        let p2 = helper.new_commit().commit().await?;
        let p3 = helper.new_commit().add_file("foo", "foo").commit().await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .add_file_with_copy_info("foo", "bar", (p3, "foo"))
            .commit()
            .await?;

        let foo = helper.lookup_filenode(commit, "foo").await?;

        assert_eq!(foo.parents(), (None, None));

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_mixed_manifest_parents(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper
            .new_commit()
            .add_file("foo/p1", "p1")
            .commit()
            .await?;

        let p2 = helper
            .new_commit()
            .add_file("foo/p2", "p2")
            .add_file("bar/p2", "p2")
            .commit()
            .await?;

        let p3 = helper
            .new_commit()
            .add_file("foo/p3", "p3")
            .add_file("bar/p3", "p3")
            .commit()
            .await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .commit()
            .await?;

        let foo = helper.lookup_manifest(commit, "foo").await?;
        let bar = helper.lookup_manifest(commit, "bar").await?;

        assert_eq!(
            foo.p1(),
            Some(helper.lookup_manifest(p1, "foo").await?.node_id())
        );

        assert_eq!(
            foo.p2(),
            Some(helper.lookup_manifest(p2, "foo").await?.node_id())
        );

        assert_eq!(
            bar.p1(),
            Some(helper.lookup_manifest(p2, "bar").await?.node_id())
        );

        assert_eq!(bar.p2(), None);

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_step_parents_metadata(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper.new_commit().commit().await?;
        let p2 = helper.new_commit().commit().await?;
        let p3 = helper.new_commit().commit().await?;
        let p4 = helper.new_commit().commit().await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .add_parent(p4)
            .commit()
            .await?;

        let hg_cs = helper.lookup_changeset(commit).await?;

        assert_eq!(
            hg_cs.p1(),
            Some(
                helper
                    .lookup_changeset(p1)
                    .await?
                    .get_changeset_id()
                    .into_nodehash()
            )
        );

        assert_eq!(
            hg_cs.p2(),
            Some(
                helper
                    .lookup_changeset(p2)
                    .await?
                    .get_changeset_id()
                    .into_nodehash()
            )
        );

        let step_parents_key: Vec<u8> = "stepparents".into();
        let step_parents = hg_cs
            .extra()
            .get(&step_parents_key)
            .ok_or_else(|| Error::msg("stepparents are missing"))?;

        assert_eq!(
            std::str::from_utf8(step_parents)?,
            format!(
                "{},{}",
                helper
                    .lookup_changeset(p3)
                    .await?
                    .get_changeset_id()
                    .to_hex(),
                helper
                    .lookup_changeset(p4)
                    .await?
                    .get_changeset_id()
                    .to_hex()
            )
        );

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_resolve_trivial_conflict(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper.new_commit().commit().await?;
        let p2 = helper.new_commit().add_file("foo", "foo").commit().await?;
        let p3 = helper
            .new_commit()
            .add_file("foo", "foo")
            .add_parent(helper.new_commit().add_file("foo", "bar").commit().await?)
            .commit()
            .await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .commit()
            .await?;

        let foo = helper.lookup_filenode(commit, "foo").await?;

        assert_eq!(
            foo.parents(),
            (
                Some(helper.lookup_filenode(p2, "foo").await?.node_id()),
                None
            )
        );

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_fail_to_resolve_conflict_on_content(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper.new_commit().commit().await?;
        let p2 = helper.new_commit().add_file("foo", "foo").commit().await?;
        let p3 = helper.new_commit().add_file("foo", "bar").commit().await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .commit()
            .await?;

        let root = helper.root_manifest(commit).await;

        let err = root
            .map(|_| ())
            .expect_err("Derivation should fail on conflict");

        assert_matches!(
            err.downcast_ref::<ErrorKind>(),
            Some(ErrorKind::UnresolvedConflicts(_, _))
        );

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_fail_to_resolve_conflict_on_type(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper.new_commit().commit().await?;
        let p2 = helper
            .new_commit()
            .add_file_with_type("foo", "foo", FileType::Regular)
            .commit()
            .await?;
        let p3 = helper
            .new_commit()
            .add_file_with_type("foo", "foo", FileType::Executable)
            .commit()
            .await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .commit()
            .await?;

        let root = helper.root_manifest(commit).await;

        let err = root
            .map(|_| ())
            .expect_err("Derivation should fail on conflict");

        assert_matches!(
            err.downcast_ref::<ErrorKind>(),
            Some(ErrorKind::UnresolvedConflicts(_, _))
        );

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_changeset_file_changes(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let p1 = helper.new_commit().commit().await?;
        let p2 = helper.new_commit().add_file("p2", "p2").commit().await?;
        let p3 = helper.new_commit().add_file("p3", "p3").commit().await?;
        let p4 = helper.new_commit().add_file("p4", "p4").commit().await?;

        let commit = helper
            .new_commit()
            .add_parent(p1)
            .add_parent(p2)
            .add_parent(p3)
            .add_parent(p4)
            .commit()
            .await?;

        let cs = helper.lookup_changeset(commit).await?;
        assert_eq!(cs.files(), &vec![MPath::new("p3")?, MPath::new("p4")?][..]);

        Ok(())
    }
}

mod case_conflicts {
    use super::*;
    use blobrepo_hg::check_case_conflicts;

    #[fbinit::compat_test]
    async fn test_internal_case_conflict(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let commit = helper
            .new_commit()
            .add_file("foo", "foo")
            .add_file("FOO", "foo")
            .commit()
            .await?;

        let id = helper.lookup_changeset(commit).await?.manifestid();

        let res = check_case_conflicts(&helper.ctx, &helper.repo, id, None).await;

        assert_matches!(
            res.unwrap_err().downcast_ref::<ErrorKind>(),
            Some(ErrorKind::InternalCaseConflict(..))
        );

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_parent_case_conflict(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let parent = helper
            .new_commit()
            .add_file("foo/bar", "foo")
            .commit()
            .await?;

        let commit = helper
            .new_commit()
            .add_parent(parent)
            .add_file("foo/BAR", "foo")
            .commit()
            .await?;

        let parent_id = helper.lookup_changeset(parent).await?.manifestid();
        let id = helper.lookup_changeset(commit).await?.manifestid();

        let res = check_case_conflicts(&helper.ctx, &helper.repo, id, Some(parent_id)).await;

        assert_matches!(
            res.unwrap_err().downcast_ref::<ErrorKind>(),
            Some(ErrorKind::ExternalCaseConflict(_, _))
        );

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_add_remove_case_conflict(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let parent = helper
            .new_commit()
            .add_file("foo/bar", "foo")
            .commit()
            .await?;

        let commit = helper
            .new_commit()
            .add_parent(parent)
            .delete_file("foo/bar")
            .add_file("FOO/bar", "foo")
            .commit()
            .await?;

        let parent_id = helper.lookup_changeset(parent).await?.manifestid();
        let id = helper.lookup_changeset(commit).await?.manifestid();

        let res = check_case_conflicts(&helper.ctx, &helper.repo, id, Some(parent_id)).await;
        assert!(res.is_ok());

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_no_case_conflict(fb: FacebookInit) -> Result<(), Error> {
        let helper = TestHelper::new(fb)?;

        let parent = helper
            .new_commit()
            .add_file("foo/bar", "foo")
            .commit()
            .await?;

        let commit = helper
            .new_commit()
            .add_parent(parent)
            .add_file("BAR/foo", "foo")
            .commit()
            .await?;

        let parent_id = helper.lookup_changeset(parent).await?.manifestid();
        let id = helper.lookup_changeset(commit).await?.manifestid();

        let res = check_case_conflicts(&helper.ctx, &helper.repo, id, Some(parent_id)).await;
        assert!(res.is_ok());

        Ok(())
    }
}
