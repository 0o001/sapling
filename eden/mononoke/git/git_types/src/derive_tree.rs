/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{bail, Error, Result};
use async_trait::async_trait;
use cloned::cloned;
use context::CoreContext;
use futures::{
    future::ready,
    stream::{FuturesUnordered, TryStreamExt},
};
use manifest::derive_manifest;
use std::convert::TryInto;

use blobstore::{Blobstore, Storable};
use derived_data::impl_bonsai_derived_via_manager;
use derived_data_manager::{dependencies, BonsaiDerivable, DerivationContext};
use filestore::{self, FetchKey};
use mononoke_types::{BonsaiChangeset, ChangesetId, MPath};

use crate::errors::ErrorKind;
use crate::{BlobHandle, Tree, TreeBuilder, TreeHandle};

fn format_key(cs_id: ChangesetId) -> String {
    format!("git.derived_root.{}", cs_id)
}

#[async_trait]
impl BonsaiDerivable for TreeHandle {
    const NAME: &'static str = "git_trees";

    type Dependencies = dependencies![];

    async fn derive_single(
        ctx: &CoreContext,
        derivation_ctx: &DerivationContext,
        bonsai: BonsaiChangeset,
        parents: Vec<Self>,
    ) -> Result<Self> {
        if bonsai.is_snapshot() {
            bail!("Can't derive TreeHandle for snapshot")
        }
        let blobstore = derivation_ctx.blobstore().clone();
        let changes = get_file_changes(&blobstore, &ctx, bonsai).await?;
        derive_git_manifest(ctx, blobstore, parents, changes).await
    }

    async fn store_mapping(
        self,
        ctx: &CoreContext,
        derivation_ctx: &DerivationContext,
        changeset_id: ChangesetId,
    ) -> Result<()> {
        let key = format_key(changeset_id);
        derivation_ctx.blobstore().put(ctx, key, self.into()).await
    }

    async fn fetch(
        ctx: &CoreContext,
        derivation_ctx: &DerivationContext,
        changeset_id: ChangesetId,
    ) -> Result<Option<Self>> {
        let key = format_key(changeset_id);
        Ok(derivation_ctx
            .blobstore()
            .get(ctx, &key)
            .await?
            .map(TryInto::try_into)
            .transpose()?)
    }
}

impl_bonsai_derived_via_manager!(TreeHandle);

async fn derive_git_manifest<B: Blobstore + Clone + 'static>(
    ctx: &CoreContext,
    blobstore: B,
    parents: Vec<TreeHandle>,
    changes: Vec<(MPath, Option<BlobHandle>)>,
) -> Result<TreeHandle, Error> {
    let handle = derive_manifest(
        ctx.clone(),
        blobstore.clone(),
        parents,
        changes,
        {
            cloned!(ctx, blobstore);
            move |tree_info| {
                let members = tree_info
                    .subentries
                    .into_iter()
                    .map(|(p, (_, entry))| (p, entry.into()))
                    .collect();

                let tree: Tree = TreeBuilder::new(members).into();

                cloned!(ctx, blobstore);
                async move {
                    let handle = tree.store(&ctx, &blobstore).await?;
                    Ok(((), handle))
                }
            }
        },
        {
            // NOTE: A None leaf will happen in derive_manifest if the parents have conflicting
            // leaves. However, since we're deriving from a Bonsai changeset and using our Git Tree
            // manifest which has leaves that are equivalent derived to their Bonsai
            // representation, that won't happen.
            |leaf_info| {
                let leaf = leaf_info
                    .leaf
                    .ok_or(ErrorKind::TreeDerivationFailed.into())
                    .map(|l| ((), l));
                ready(leaf)
            }
        },
    )
    .await?;

    match handle {
        Some(handle) => Ok(handle),
        None => {
            let tree: Tree = TreeBuilder::default().into();
            tree.store(&ctx, &blobstore).await
        }
    }
}

pub async fn get_file_changes<B: Blobstore + Clone>(
    blobstore: &B,
    ctx: &CoreContext,
    bcs: BonsaiChangeset,
) -> Result<Vec<(MPath, Option<BlobHandle>)>, Error> {
    bcs.into_mut()
        .file_changes
        .into_iter()
        .map(|(mpath, file_change)| {
            cloned!(ctx, blobstore);
            async move {
                match file_change.simplify() {
                    Some(fc) => {
                        let t = fc.file_type();
                        let k = FetchKey::Canonical(fc.content_id());

                        let r = filestore::get_metadata(&blobstore, &ctx, &k).await?;
                        let m = r.ok_or(ErrorKind::ContentMissing(k))?;
                        Ok((mpath, Some(BlobHandle::new(m, t))))
                    }
                    None => Ok((mpath, None)),
                }
            }
        })
        .collect::<FuturesUnordered<_>>()
        .try_collect()
        .await
}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::format_err;
    use blobrepo::BlobRepo;
    use derived_data::BonsaiDerived;
    use fbinit::FacebookInit;
    use filestore::Alias;
    use futures_util::stream::TryStreamExt;
    use git2::{Oid, Repository};
    use manifest::ManifestOps;
    use std::fs::File;
    use std::io::Write;
    use std::path::Path;
    use tempdir::TempDir;

    /// This function creates a new Git tree from the fixture's master Bonsai bookmark,
    /// materializes it to disk, then verifies that libgit produces the same Git tree for it.
    async fn run_tree_derivation_for_fixture(
        fb: FacebookInit,
        repo: BlobRepo,
    ) -> Result<(), Error> {
        let ctx = CoreContext::test_mock(fb);

        let bcs_id = repo
            .get_bonsai_bookmark(ctx.clone(), &("master".try_into()?))
            .await?
            .ok_or(format_err!("no master"))?;

        let tree = TreeHandle::derive(&ctx, &repo, bcs_id).await?;

        let leaves = tree
            .list_leaf_entries(ctx.clone(), repo.get_blobstore())
            .try_collect::<Vec<_>>()
            .await?;

        let tmp_dir = TempDir::new("git_types_test")?;
        let root_path = tmp_dir.path();
        let git = Repository::init(&root_path)?;
        let mut index = git.index()?;

        for (mpath, blob_handle) in leaves.into_iter() {
            let blob = filestore::fetch_concat(
                &repo.get_blobstore(),
                &ctx,
                FetchKey::Aliased(Alias::GitSha1(blob_handle.oid().sha1())),
            )
            .await?;

            let path = &mpath.to_string();
            let path = Path::new(&path);
            File::create(&root_path.join(&path))?.write_all(&blob)?;

            index.add_path(&path)?;
        }

        let git_oid = index.write_tree()?;
        let derived_tree_oid = Oid::from_bytes(tree.oid().as_ref())?;
        assert_eq!(git_oid, derived_tree_oid);

        tmp_dir.close()?;

        Ok(())
    }

    macro_rules! impl_test {
        ($fixture:ident) => {
            #[fbinit::test]
            fn $fixture(fb: FacebookInit) -> Result<(), Error> {
                let runtime = tokio::runtime::Runtime::new()?;
                runtime.block_on(async move {
                    let repo = fixtures::$fixture::getrepo(fb).await;
                    run_tree_derivation_for_fixture(fb, repo).await
                })
            }
        };
    }

    impl_test!(linear);
    impl_test!(branch_even);
    impl_test!(branch_uneven);
    impl_test!(branch_wide);
    impl_test!(merge_even);
    impl_test!(many_files_dirs);
    impl_test!(merge_uneven);
    impl_test!(unshared_merge_even);
    impl_test!(unshared_merge_uneven);
    impl_test!(many_diamonds);
}
