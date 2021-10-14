/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::derive::{derive_unode_manifest, derive_unode_manifest_stack};
use anyhow::{Context, Error, Result};
use async_trait::async_trait;
use blobstore::{Blobstore, BlobstoreGetData, Loadable};
use bytes::Bytes;
use context::CoreContext;
use derived_data::batch::{split_bonsais_in_linear_stacks, FileConflicts};
use derived_data::impl_bonsai_derived_via_manager;
use derived_data_manager::{dependencies, BonsaiDerivable, DerivationContext};
use futures::{future::try_join_all, TryFutureExt};
use metaconfig_types::UnodeVersion;
use mononoke_types::{
    BlobstoreBytes, BonsaiChangeset, ChangesetId, ContentId, FileType, MPath, ManifestUnodeId,
};
use slog::debug;
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct RootUnodeManifestId(ManifestUnodeId);

impl RootUnodeManifestId {
    pub fn manifest_unode_id(&self) -> &ManifestUnodeId {
        &self.0
    }
}

impl TryFrom<BlobstoreBytes> for RootUnodeManifestId {
    type Error = Error;

    fn try_from(blob_bytes: BlobstoreBytes) -> Result<Self> {
        ManifestUnodeId::from_bytes(&blob_bytes.into_bytes()).map(RootUnodeManifestId)
    }
}

impl TryFrom<BlobstoreGetData> for RootUnodeManifestId {
    type Error = Error;

    fn try_from(blob_val: BlobstoreGetData) -> Result<Self> {
        blob_val.into_bytes().try_into()
    }
}

impl From<RootUnodeManifestId> for BlobstoreBytes {
    fn from(root_mf_id: RootUnodeManifestId) -> Self {
        BlobstoreBytes::from_bytes(Bytes::copy_from_slice(root_mf_id.0.blake2().as_ref()))
    }
}

fn format_key(derivation_ctx: &DerivationContext, changeset_id: ChangesetId) -> String {
    let prefix = match derivation_ctx.config().unode_version {
        UnodeVersion::V1 => "derived_root_unode.",
        UnodeVersion::V2 => "derived_root_unode_v2.",
    };
    format!("{}{}", prefix, changeset_id)
}

#[async_trait]
impl BonsaiDerivable for RootUnodeManifestId {
    const NAME: &'static str = "unodes";

    type Dependencies = dependencies![];

    async fn derive_single(
        ctx: &CoreContext,
        derivation_ctx: &DerivationContext,
        bonsai: BonsaiChangeset,
        parents: Vec<Self>,
    ) -> Result<Self> {
        let unode_version = derivation_ctx.config().unode_version;
        let csid = bonsai.get_changeset_id();
        derive_unode_manifest(
            ctx,
            derivation_ctx,
            csid,
            parents
                .into_iter()
                .map(|root_mf_id| root_mf_id.manifest_unode_id().clone())
                .collect(),
            get_file_changes(&bonsai),
            unode_version,
        )
        .map_ok(RootUnodeManifestId)
        .await
    }

    async fn derive_batch(
        ctx: &CoreContext,
        derivation_ctx: &DerivationContext,
        bonsais: Vec<BonsaiChangeset>,
        _gap_size: Option<usize>,
    ) -> Result<HashMap<ChangesetId, Self>> {
        if bonsais.is_empty() {
            return Ok(HashMap::new());
        }

        let mut res = HashMap::new();
        if !tunables::tunables().get_unodes_use_batch_derivation() {
            for bonsai in bonsais {
                let csid = bonsai.get_changeset_id();
                let parents = derivation_ctx
                    .fetch_unknown_parents(ctx, Some(&res), &bonsai)
                    .await?;
                let derived = Self::derive_single(ctx, derivation_ctx, bonsai, parents).await?;
                res.insert(csid, derived);
            }
            return Ok(res);
        }

        let batch_len = bonsais.len();
        let stacks = split_bonsais_in_linear_stacks(&bonsais, FileConflicts::ChangeDelete)?;

        let unode_version = derivation_ctx.config().unode_version;
        for stack in stacks {
            let derived_parents = try_join_all(
                stack
                    .parents
                    .into_iter()
                    .map(|p| derivation_ctx.fetch_unknown_dependency::<Self>(&ctx, Some(&res), p)),
            )
            .await?;
            if let Some(item) = stack.file_changes.first() {
                debug!(
                    ctx.logger(),
                    "derive unode batch at {} (stack of {} from batch of {})",
                    item.cs_id.to_hex(),
                    stack.file_changes.len(),
                    batch_len,
                );
            }

            if derived_parents.len() > 1 {
                // we can't derive stack for a merge commit,
                // so let's derive it without batching
                for item in stack.file_changes {
                    let bonsai = item.cs_id.load(&ctx, derivation_ctx.blobstore()).await?;
                    let parents = derivation_ctx
                        .fetch_unknown_parents(ctx, Some(&res), &bonsai)
                        .await?;
                    let derived = Self::derive_single(ctx, derivation_ctx, bonsai, parents).await?;
                    res.insert(item.cs_id, derived);
                }
            } else {
                let first = stack.file_changes.first().map(|item| item.cs_id);
                let last = stack.file_changes.last().map(|item| item.cs_id);
                let derived = derive_unode_manifest_stack(
                    ctx,
                    derivation_ctx,
                    stack
                        .file_changes
                        .into_iter()
                        .map(|item| (item.cs_id, item.per_commit_file_changes))
                        .collect(),
                    derived_parents
                        .get(0)
                        .map(|mf_id| *mf_id.manifest_unode_id()),
                    unode_version,
                )
                .await
                .with_context(|| format!("failed deriving stack of {:?} to {:?}", first, last,))?;

                res.extend(derived.into_iter().map(|(csid, mf_id)| (csid, Self(mf_id))));
            }
        }

        Ok(res)
    }

    async fn store_mapping(
        self,
        ctx: &CoreContext,
        derivation_ctx: &DerivationContext,
        changeset_id: ChangesetId,
    ) -> Result<()> {
        let key = format_key(derivation_ctx, changeset_id);
        derivation_ctx.blobstore().put(ctx, key, self.into()).await
    }

    async fn fetch(
        ctx: &CoreContext,
        derivation_ctx: &DerivationContext,
        changeset_id: ChangesetId,
    ) -> Result<Option<Self>> {
        let key = format_key(derivation_ctx, changeset_id);
        match derivation_ctx.blobstore().get(ctx, &key).await? {
            Some(blob) => Ok(Some(blob.try_into()?)),
            None => Ok(None),
        }
    }
}

// For existing users of BonsaiDerived.
impl_bonsai_derived_via_manager!(RootUnodeManifestId);

pub(crate) fn get_file_changes(
    bcs: &BonsaiChangeset,
) -> Vec<(MPath, Option<(ContentId, FileType)>)> {
    bcs.file_changes()
        .map(|(mpath, file_change)| {
            let content_file_type = file_change
                .simplify()
                .map(|bc| (bc.content_id(), bc.file_type()));
            (mpath.clone(), content_file_type)
        })
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;
    use blobrepo::BlobRepo;
    use blobrepo_hg::BlobRepoHg;
    use blobstore::Loadable;
    use bookmarks::BookmarkName;
    use borrowed::borrowed;
    use cloned::cloned;
    use derived_data::BonsaiDerived;
    use derived_data_manager::BatchDeriveOptions;
    use derived_data_test_utils::iterate_all_manifest_entries;
    use fbinit::FacebookInit;
    use fixtures::{
        branch_even, branch_uneven, branch_wide, linear, many_diamonds, many_files_dirs,
        merge_even, merge_uneven, unshared_merge_even, unshared_merge_uneven,
    };
    use futures::{compat::Stream01CompatExt, Future, FutureExt, Stream, TryStreamExt};
    use manifest::Entry;
    use maplit::hashmap;
    use mercurial_types::{HgChangesetId, HgManifestId};
    use mononoke_types::ChangesetId;
    use repo_derived_data::RepoDerivedDataRef;
    use revset::AncestorsNodeStream;
    use tests_utils::CreateCommitContext;

    async fn fetch_manifest_by_cs_id(
        ctx: &CoreContext,
        repo: &BlobRepo,
        hg_cs_id: HgChangesetId,
    ) -> Result<HgManifestId, Error> {
        Ok(hg_cs_id.load(ctx, repo.blobstore()).await?.manifestid())
    }

    async fn verify_unode(
        ctx: &CoreContext,
        repo: &BlobRepo,
        bcs_id: ChangesetId,
        hg_cs_id: HgChangesetId,
    ) -> Result<RootUnodeManifestId, Error> {
        let (unode_entries, mf_unode_id) = async move {
            let mf_unode_id = RootUnodeManifestId::derive(ctx, repo, bcs_id)
                .await?
                .manifest_unode_id()
                .clone();
            let mut paths = iterate_all_manifest_entries(ctx, repo, Entry::Tree(mf_unode_id))
                .map_ok(|(path, _)| path)
                .try_collect::<Vec<_>>()
                .await?;
            paths.sort();
            Result::<_, Error>::Ok((paths, RootUnodeManifestId(mf_unode_id)))
        }
        .await?;

        let filenode_entries = async move {
            let root_mf_id = fetch_manifest_by_cs_id(ctx, repo, hg_cs_id).await?;
            let mut paths = iterate_all_manifest_entries(ctx, repo, Entry::Tree(root_mf_id))
                .map_ok(|(path, _)| path)
                .try_collect::<Vec<_>>()
                .await?;
            paths.sort();
            Result::<_, Error>::Ok(paths)
        };

        let filenode_entries = filenode_entries.await?;
        assert_eq!(unode_entries, filenode_entries);

        Ok(mf_unode_id)
    }

    fn all_commits_descendants_to_ancestors(
        ctx: CoreContext,
        repo: BlobRepo,
    ) -> impl Stream<Item = Result<(ChangesetId, HgChangesetId), Error>> {
        let master_book = BookmarkName::new("master").unwrap();
        repo.get_bonsai_bookmark(ctx.clone(), &master_book)
            .map_ok(move |maybe_bcs_id| {
                let bcs_id = maybe_bcs_id.unwrap();
                AncestorsNodeStream::new(ctx.clone(), &repo.get_changeset_fetcher(), bcs_id.clone())
                    .compat()
                    .and_then(move |new_bcs_id| {
                        cloned!(ctx, repo);
                        async move {
                            let hg_cs_id = repo
                                .get_hg_from_bonsai_changeset(ctx.clone(), new_bcs_id)
                                .await?;
                            Result::<_, Error>::Ok((new_bcs_id, hg_cs_id))
                        }
                    })
            })
            .try_flatten_stream()
    }

    async fn verify_repo<F, Fut>(fb: FacebookInit, repo_func: F)
    where
        F: Fn() -> Fut,
        Fut: Future<Output = BlobRepo>,
    {
        let ctx = CoreContext::test_mock(fb);
        let repo = repo_func().await;
        println!("Processing {}", repo.name());
        borrowed!(ctx, repo);

        let commits_desc_to_anc = all_commits_descendants_to_ancestors(ctx.clone(), repo.clone())
            .and_then(move |(bcs_id, hg_cs_id)| async move {
                let unode_id = verify_unode(&ctx, &repo, bcs_id, hg_cs_id).await?;
                Ok((bcs_id, hg_cs_id, unode_id))
            })
            .try_collect::<Vec<_>>()
            .await
            .unwrap();

        // Recreate repo from scratch and derive everything again
        let repo = repo_func().await;
        let options = BatchDeriveOptions::Parallel { gap_size: None };
        let csids = commits_desc_to_anc
            .clone()
            .into_iter()
            .rev()
            .map(|(cs_id, _, _)| cs_id)
            .collect::<Vec<_>>();
        let manager = repo.repo_derived_data().manager();

        let tunables = tunables::MononokeTunables::default();
        tunables.update_bools(&hashmap! {"unodes_use_batch_derivation".to_string() => true});

        let batch_derived = tunables::with_tunables_async(
            tunables,
            async {
                manager
                    .backfill_batch::<RootUnodeManifestId>(&ctx, csids.clone(), options, None)
                    .await?;
                manager
                    .fetch_derived_batch::<RootUnodeManifestId>(&ctx, csids, None)
                    .await
            }
            .boxed(),
        )
        .await
        .unwrap();

        for (cs_id, hg_cs_id, unode_id) in commits_desc_to_anc.into_iter().rev() {
            println!("{} {}", cs_id, hg_cs_id);
            println!("{:?} {:?}", batch_derived.get(&cs_id), Some(&unode_id));
            assert_eq!(batch_derived.get(&cs_id), Some(&unode_id));
        }
    }

    #[fbinit::test]
    async fn test_unode_derivation_on_multiple_repos(fb: FacebookInit) {
        verify_repo(fb, || linear::getrepo(fb)).await;
        verify_repo(fb, || branch_even::getrepo(fb)).await;
        verify_repo(fb, || branch_uneven::getrepo(fb)).await;
        verify_repo(fb, || branch_wide::getrepo(fb)).await;
        verify_repo(fb, || many_diamonds::getrepo(fb)).await;
        verify_repo(fb, || many_files_dirs::getrepo(fb)).await;
        verify_repo(fb, || merge_even::getrepo(fb)).await;
        verify_repo(fb, || merge_uneven::getrepo(fb)).await;
        verify_repo(fb, || unshared_merge_even::getrepo(fb)).await;
        verify_repo(fb, || unshared_merge_uneven::getrepo(fb)).await;
        // Create a repo with a few empty commits in a row
        verify_repo(fb, || async {
            let repo: BlobRepo = test_repo_factory::build_empty().unwrap();
            let ctx = CoreContext::test_mock(fb);
            let root_empty = CreateCommitContext::new_root(&ctx, &repo)
                .commit()
                .await
                .unwrap();
            let first_empty = CreateCommitContext::new(&ctx, &repo, vec![root_empty])
                .commit()
                .await
                .unwrap();
            let second_empty = CreateCommitContext::new(&ctx, &repo, vec![first_empty])
                .commit()
                .await
                .unwrap();
            let first_non_empty = CreateCommitContext::new(&ctx, &repo, vec![second_empty])
                .add_file("file", "a")
                .commit()
                .await
                .unwrap();
            let third_empty = CreateCommitContext::new(&ctx, &repo, vec![first_non_empty])
                .delete_file("file")
                .commit()
                .await
                .unwrap();
            let fourth_empty = CreateCommitContext::new(&ctx, &repo, vec![third_empty])
                .commit()
                .await
                .unwrap();
            let fifth_empty = CreateCommitContext::new(&ctx, &repo, vec![fourth_empty])
                .commit()
                .await
                .unwrap();

            tests_utils::bookmark(&ctx, &repo, "master")
                .set_to(fifth_empty)
                .await
                .unwrap();
            repo
        })
        .await;

        verify_repo(fb, || async {
            let repo: BlobRepo = test_repo_factory::build_empty().unwrap();
            let ctx = CoreContext::test_mock(fb);
            let root = CreateCommitContext::new_root(&ctx, &repo)
                .add_file("dir/subdir/to_replace", "one")
                .add_file("dir/subdir/file", "content")
                .add_file("somefile", "somecontent")
                .commit()
                .await
                .unwrap();
            let modify_unrelated = CreateCommitContext::new(&ctx, &repo, vec![root])
                .add_file("dir/subdir/file", "content2")
                .delete_file("somefile")
                .commit()
                .await
                .unwrap();
            let replace_file_with_dir =
                CreateCommitContext::new(&ctx, &repo, vec![modify_unrelated])
                    .delete_file("dir/subdir/to_replace")
                    .add_file("dir/subdir/to_replace/file", "newcontent")
                    .commit()
                    .await
                    .unwrap();

            tests_utils::bookmark(&ctx, &repo, "master")
                .set_to(replace_file_with_dir)
                .await
                .unwrap();
            repo
        })
        .await;

        // Weird case - let's delete a file that was already replaced with a directory
        verify_repo(fb, || async {
            let repo: BlobRepo = test_repo_factory::build_empty().unwrap();
            let ctx = CoreContext::test_mock(fb);
            let root = CreateCommitContext::new_root(&ctx, &repo)
                .add_file("dir/subdir/to_replace", "one")
                .commit()
                .await
                .unwrap();
            let replace_file_with_dir = CreateCommitContext::new(&ctx, &repo, vec![root])
                .delete_file("dir/subdir/to_replace")
                .add_file("dir/subdir/to_replace/file", "newcontent")
                .commit()
                .await
                .unwrap();
            let noop_delete = CreateCommitContext::new(&ctx, &repo, vec![replace_file_with_dir])
                .delete_file("dir/subdir/to_replace")
                .commit()
                .await
                .unwrap();
            let second_noop_delete = CreateCommitContext::new(&ctx, &repo, vec![noop_delete])
                .delete_file("dir/subdir/to_replace")
                .commit()
                .await
                .unwrap();

            tests_utils::bookmark(&ctx, &repo, "master")
                .set_to(second_noop_delete)
                .await
                .unwrap();
            repo
        })
        .await;
    }
}
