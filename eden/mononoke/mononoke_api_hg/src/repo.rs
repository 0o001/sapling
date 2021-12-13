/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{self, format_err, Context, Error};
use blobrepo::BlobRepo;
use blobrepo_hg::{save_bonsai_changeset_object, BlobRepoHg, ChangesetHandle};
use blobstore::{Blobstore, Loadable, LoadableError};
use bookmarks::Freshness;
use bytes::Bytes;
use changesets::{ChangesetInsert, Changesets};
use context::{CoreContext, SessionClass};
use edenapi_types::{AnyId, UploadToken};
use ephemeral_blobstore::{Bubble, BubbleId, RepoEphemeralStore, StorageLocation};
use filestore::{self, Alias, FetchKey, StoreRequest};
use futures::compat::{Future01CompatExt, Stream01CompatExt};
use futures::{future, stream, Stream, StreamExt, TryStream, TryStreamExt};
use futures_util::try_join;
use hgproto::GettreepackArgs;
use mercurial_mutation::HgMutationEntry;
use mercurial_types::blobs::{RevlogChangeset, UploadHgNodeHash, UploadHgTreeEntry};
use mercurial_types::{HgChangesetId, HgFileEnvelopeMut, HgFileNodeId, HgManifestId, HgNodeHash};
use metaconfig_types::RepoConfig;
use mononoke_api::RepoWriteContext;
use mononoke_api::{errors::MononokeError, path::MononokePath, repo::RepoContext};
use mononoke_types::{
    hash::{Sha1, Sha256},
    BonsaiChangeset, ChangesetId, ContentId, ContentMetadata, MPath, MononokeId, RepoPath,
};
use phases::PhasesRef;
use reachabilityindex::LeastCommonAncestorsHint;
use repo_blobstore::RepoBlobstore;
use repo_client::{
    find_commits_to_send, find_new_draft_commits_and_derive_filenodes_for_public_roots,
    gettreepack_entries,
};
use segmented_changelog::{CloneData, Location};
use tunables::tunables;
use unbundle::upload_changeset;

use super::{HgFileContext, HgTreeContext};

#[derive(Clone)]
pub struct HgRepoContext {
    repo: RepoContext,
}

impl HgRepoContext {
    pub(crate) fn new(repo: RepoContext) -> Self {
        Self { repo }
    }

    /// The `CoreContext` for this query.
    pub fn ctx(&self) -> &CoreContext {
        &self.repo.ctx()
    }

    /// The `RepoContext` for this query.
    pub fn repo(&self) -> &RepoContext {
        &self.repo
    }

    /// The underlying Mononoke `BlobRepo` backing this repo.
    pub(crate) fn blob_repo(&self) -> &BlobRepo {
        &self.repo().blob_repo()
    }

    /// The configuration for the repository.
    pub(crate) fn config(&self) -> &RepoConfig {
        self.repo.config()
    }

    /// Create bubble and return its id
    pub async fn create_bubble(
        &self,
        custom_duration: Option<Duration>,
    ) -> Result<Bubble, MononokeError> {
        Ok(self
            .repo()
            .ephemeral_store()
            .create_bubble(custom_duration)
            .await?)
    }

    /// Get a write context to make changes to this repository.
    pub async fn write(self) -> Result<RepoWriteContext, MononokeError> {
        self.repo.write().await
    }

    pub fn ephemeral_store(&self) -> &Arc<RepoEphemeralStore> {
        self.repo().ephemeral_store()
    }


    /// Load bubble from id
    pub async fn open_bubble(&self, bubble_id: BubbleId) -> Result<Bubble, MononokeError> {
        Ok(self.repo.open_bubble(bubble_id).await?)
    }

    /// Get blobstore. If bubble id is present, this is the ephemeral blobstore
    pub async fn bubble_blobstore(
        &self,
        bubble_id: Option<BubbleId>,
    ) -> Result<RepoBlobstore, MononokeError> {
        let main_blobstore = self.blob_repo().blobstore().clone();
        Ok(match bubble_id {
            Some(id) => self
                .repo
                .open_bubble(id)
                .await?
                .wrap_repo_blobstore(main_blobstore),
            None => main_blobstore,
        })
    }

    /// Get changeset id from hg changeset id
    pub async fn get_bonsai_from_hg(
        &self,
        hgid: HgChangesetId,
    ) -> Result<Option<ChangesetId>, MononokeError> {
        Ok(self
            .blob_repo()
            .get_bonsai_from_hg(self.ctx().clone(), hgid)
            .await?)
    }

    /// Fetch file content size, fails if it doesn't exist.
    pub async fn fetch_file_content_size(
        &self,
        content_id: ContentId,
        bubble_id: Option<BubbleId>,
    ) -> Result<u64, MononokeError> {
        Ok(filestore::get_metadata(
            &self.bubble_blobstore(bubble_id).await?,
            self.ctx(),
            &FetchKey::Canonical(content_id),
        )
        .await?
        .ok_or_else(|| {
            MononokeError::InvalidRequest(format!(
                "failed to fetch or rebuild metadata for ContentId('{}'), file content must be prior uploaded",
                content_id
            ))
        })?
        .total_size)
    }

    async fn is_key_present_in_blobstore(&self, key: &str) -> Result<bool, MononokeError> {
        async move {
            let mut ctx = self.ctx().clone();
            let is_comprehensive = tunables().get_edenapi_lookup_use_comprehensive_mode();
            if is_comprehensive {
                ctx.session_mut()
                    .override_session_class(SessionClass::ComprehensiveLookup);
            }
            self.blob_repo()
                .blobstore()
                .is_present(&ctx, &key)
                .await
                .map(|is_present| {
                    // if we can't resolve the presence (some blobstores failed, some returned None)
                    // we can re-upload the blob
                    is_present.assume_not_found_if_unsure()
                })
        }
        .await
        .map_err(MononokeError::from)
    }

    /// Look up in blobstore by `ContentId`
    pub async fn is_file_present_by_contentid(
        &self,
        content_id: ContentId,
    ) -> Result<bool, MononokeError> {
        self.is_key_present_in_blobstore(&content_id.blobstore_key())
            .await
    }

    /// Look up in blobstore by `Sha1 alias`
    pub async fn is_file_present_by_sha1(&self, sha1: Sha1) -> Result<bool, MononokeError> {
        self.is_key_present_in_blobstore(&Alias::Sha1(sha1).blobstore_key())
            .await
    }

    /// Convert given hash to canonical ContentId
    pub async fn convert_file_to_content_id<H: Into<FetchKey> + Copy + std::fmt::Debug>(
        &self,
        hash: H,
        bubble_id: Option<BubbleId>,
    ) -> Result<Option<ContentId>, MononokeError> {
        match hash
            .into()
            .load(self.ctx(), &self.bubble_blobstore(bubble_id).await?)
            .await
        {
            Ok(cid) => Ok(Some(cid)),
            Err(LoadableError::Missing(_)) => Ok(None),
            Err(LoadableError::Error(err)) => {
                Err(err).with_context(|| format_err!("While fetching ContentId for {:?}", hash))?
            }
        }
    }

    /// Store file into blobstore
    pub async fn store_file(
        &self,
        key: impl Into<FetchKey>,
        size: u64,
        data: impl Stream<Item = Result<Bytes, Error>> + Send,
        bubble_id: Option<BubbleId>,
    ) -> Result<ContentMetadata, MononokeError> {
        filestore::store(
            &self.bubble_blobstore(bubble_id).await?,
            self.blob_repo().filestore_config(),
            self.ctx(),
            &StoreRequest::with_fetch_key(size, key.into()),
            data,
        )
        .await
        .map_err(MononokeError::from)
    }

    /// Look up in blobstore by `Sha256 alias`
    pub async fn is_file_present_by_sha256(&self, sha256: Sha256) -> Result<bool, MononokeError> {
        self.is_key_present_in_blobstore(&Alias::Sha256(sha256).blobstore_key())
            .await
    }


    /// Download file contents
    pub async fn download_file(
        &self,
        upload_token: UploadToken,
    ) -> Result<Option<impl Stream<Item = Result<Bytes, Error>> + 'static>, MononokeError> {
        Ok(filestore::fetch(
            self.bubble_blobstore(upload_token.data.bubble_id.map(BubbleId::new))
                .await?,
            self.ctx().clone(),
            &match upload_token.data.id {
                AnyId::AnyFileContentId(file_id) => file_id.into(),
                e @ _ => {
                    return Err(MononokeError::from(format_err!(
                        "Id is not of a file: {:?}",
                        e
                    )));
                }
            },
        )
        .await?)
    }

    /// Test whether a Mercurial changeset exists.
    pub async fn hg_changeset_exists(
        &self,
        hg_changeset_id: HgChangesetId,
    ) -> Result<bool, MononokeError> {
        self.blob_repo()
            .changeset_exists(self.ctx().clone(), hg_changeset_id)
            .await
            .map_err(MononokeError::from)
    }

    /// Test whether a changeset exists in a particular storage location.
    pub async fn changeset_exists(
        &self,
        changeset_id: ChangesetId,
        storage_location: StorageLocation,
    ) -> Result<bool, MononokeError> {
        Ok(self
            .repo
            .changeset_exists(changeset_id, storage_location)
            .await?)
    }

    /// Look up in blobstore by `HgFileNodeId`
    pub async fn filenode_exists(&self, filenode_id: HgFileNodeId) -> Result<bool, MononokeError> {
        self.is_key_present_in_blobstore(&filenode_id.blobstore_key())
            .await
    }

    /// Look up in blobstore by `HgManifestId`
    pub async fn tree_exists(&self, manifest_id: HgManifestId) -> Result<bool, MononokeError> {
        self.is_key_present_in_blobstore(&manifest_id.blobstore_key())
            .await
    }

    /// Look up a file in the repo by `HgFileNodeId`.
    pub async fn file(
        &self,
        filenode_id: HgFileNodeId,
    ) -> Result<Option<HgFileContext>, MononokeError> {
        HgFileContext::new_check_exists(self.clone(), filenode_id).await
    }

    /// Look up a tree in the repo by `HgManifestId`.
    pub async fn tree(
        &self,
        manifest_id: HgManifestId,
    ) -> Result<Option<HgTreeContext>, MononokeError> {
        HgTreeContext::new_check_exists(self.clone(), manifest_id).await
    }


    /// Store HgFilenode into blobstore
    pub async fn store_hg_filenode(
        &self,
        filenode_id: HgFileNodeId,
        p1: Option<HgFileNodeId>,
        p2: Option<HgFileNodeId>,
        content_id: ContentId,
        content_size: u64,
        metadata: Bytes,
    ) -> Result<(), MononokeError> {
        let envelope = HgFileEnvelopeMut {
            node_id: filenode_id,
            p1,
            p2,
            content_id,
            content_size,
            metadata,
        };

        self.blob_repo()
            .blobstore()
            .put(
                self.ctx(),
                filenode_id.blobstore_key(),
                envelope.freeze().into_blob().into(),
            )
            .await
            .map_err(MononokeError::from)?;
        Ok(())
    }

    /// Store Tree into blobstore
    pub async fn store_tree(
        &self,
        upload_node_id: HgNodeHash,
        p1: Option<HgNodeHash>,
        p2: Option<HgNodeHash>,
        contents: Bytes,
    ) -> Result<(), MononokeError> {
        let entry = UploadHgTreeEntry {
            upload_node_id: UploadHgNodeHash::Checked(upload_node_id),
            contents,
            p1,
            p2,
            path: RepoPath::RootPath, // only used for logging
        };
        let (_, upload_future) = entry.upload(
            self.ctx().clone(),
            Arc::new(self.blob_repo().blobstore().clone()),
        )?;

        upload_future.compat().await.map_err(MononokeError::from)?;

        Ok(())
    }


    /// Store HgChangeset. The function also generates bonsai changeset and stores all necessary mappings.
    pub async fn store_hg_changesets(
        &self,
        changesets: Vec<(HgChangesetId, RevlogChangeset)>,
        mutations: Vec<HgMutationEntry>,
    ) -> Result<Vec<Result<(HgChangesetId, BonsaiChangeset), MononokeError>>, MononokeError> {
        let mut uploaded_changesets: HashMap<HgChangesetId, ChangesetHandle> = HashMap::new();
        let filelogs = HashMap::new();
        let manifests = HashMap::new();
        for (node, revlog_cs) in changesets {
            uploaded_changesets = upload_changeset(
                self.ctx().clone(),
                self.blob_repo().clone(),
                self.ctx().scuba().clone(),
                node,
                &revlog_cs,
                uploaded_changesets,
                &filelogs,
                &manifests,
                None, /* maybe_backup_repo_source (unsupported here) */
            )
            .await
            .map_err(MononokeError::from)?;
        }
        let mut results = vec![];
        let mut hg_changesets = HashSet::new();
        for (hg_cs_id, handle) in uploaded_changesets {
            let result = handle
                .get_completed_changeset()
                .await
                .map_err(Error::from)
                .map_err(MononokeError::from)
                .map(|(bonsai, _)| (hg_cs_id, bonsai));
            if result.is_ok() {
                hg_changesets.insert(hg_cs_id);
            }
            results.push(result);
        }
        self.blob_repo()
            .hg_mutation_store()
            .add_entries(&self.ctx(), hg_changesets, mutations)
            .await
            .map_err(MononokeError::from)?;

        Ok(results)
    }

    pub async fn fetch_mutations(
        &self,
        hg_changesets: HashSet<HgChangesetId>,
    ) -> Result<Vec<HgMutationEntry>, MononokeError> {
        Ok(self
            .blob_repo()
            .hg_mutation_store()
            .all_predecessors(self.ctx(), hg_changesets)
            .await?)
    }

    /// Store bonsai changeset
    pub async fn store_bonsai_changeset(
        &self,
        bonsai_cs: BonsaiChangeset,
    ) -> Result<(), MononokeError> {
        let blobstore = self.blob_repo().blobstore();
        let cs_id = bonsai_cs.get_changeset_id();
        let insert = ChangesetInsert {
            cs_id,
            parents: bonsai_cs.parents().collect(),
        };
        match save_bonsai_changeset_object(&self.ctx(), blobstore, bonsai_cs).await {
            Ok(_) => {
                self.blob_repo()
                    .get_changesets_object()
                    .add(self.ctx().clone(), insert)
                    .await
            }
            Err(err) => Err(err),
        }?;

        Ok(())
    }

    /// Request all of the tree nodes in the repo under a given path.
    ///
    /// The caller must specify a list of desired versions of the subtree for
    /// this path, specified as a list of manifest IDs of tree nodes
    /// corresponding to different versions of the root node of the subtree.
    ///
    /// The caller may also specify a list of versions of the subtree to
    /// delta against. The server will only return tree nodes that are in
    /// the requested subtrees that are not in the base subtrees.
    ///
    /// Returns a stream of `HgTreeContext`s, each corresponding to a node in
    /// the requested versions of the subtree, along with its associated path.
    ///
    /// This method is equivalent to Mercurial's `gettreepack` wire protocol
    /// command.
    pub fn trees_under_path(
        &self,
        path: MononokePath,
        root_versions: impl IntoIterator<Item = HgManifestId>,
        base_versions: impl IntoIterator<Item = HgManifestId>,
        depth: Option<usize>,
    ) -> impl TryStream<Ok = (HgTreeContext, MononokePath), Error = MononokeError> {
        let ctx = self.ctx().clone();
        let blob_repo = self.blob_repo();
        let args = GettreepackArgs {
            rootdir: path.into_mpath(),
            mfnodes: root_versions.into_iter().collect(),
            basemfnodes: base_versions.into_iter().collect(),
            directories: vec![], // Not supported.
            depth,
        };

        gettreepack_entries(ctx, blob_repo, args)
            .compat()
            .map_err(MononokeError::from)
            .and_then({
                let repo = self.clone();
                move |(mfid, path): (HgManifestId, Option<MPath>)| {
                    let repo = repo.clone();
                    async move {
                        let tree = HgTreeContext::new(repo, mfid).await?;
                        let path = MononokePath::new(path);
                        Ok((tree, path))
                    }
                }
            })
    }

    pub async fn get_hg_from_bonsai(
        &self,
        cs_id: ChangesetId,
    ) -> Result<HgChangesetId, MononokeError> {
        Ok(self
            .blob_repo()
            .get_hg_from_bonsai_changeset(self.ctx().clone(), cs_id)
            .await?)
    }

    /// This provides the same functionality as
    /// `mononoke_api::RepoContext::location_to_changeset_id`. It just wraps the request and
    /// response using Mercurial specific types.
    pub async fn location_to_hg_changeset_id(
        &self,
        location: Location<HgChangesetId>,
        count: u64,
    ) -> Result<Vec<HgChangesetId>, MononokeError> {
        let cs_location = location
            .and_then_descendant(|descendant| async move {
                self.blob_repo()
                    .get_bonsai_from_hg(self.ctx().clone(), descendant)
                    .await?
                    .ok_or_else(|| {
                        MononokeError::InvalidRequest(format!(
                            "hg changeset {} not found",
                            location.descendant
                        ))
                    })
            })
            .await?;
        let result_csids = self
            .repo()
            .location_to_changeset_id(cs_location, count)
            .await?;
        let hg_id_futures = result_csids.iter().map(|result_csid| {
            self.blob_repo()
                .get_hg_from_bonsai_changeset(self.ctx().clone(), *result_csid)
        });
        future::try_join_all(hg_id_futures)
            .await
            .map_err(MononokeError::from)
    }

    /// This provides the same functionality as
    /// `mononke_api::RepoContext::many_changeset_ids_to_locations`. It just translates to
    /// and from Mercurial types.
    pub async fn many_changeset_ids_to_locations(
        &self,
        hg_master_heads: Vec<HgChangesetId>,
        hg_ids: Vec<HgChangesetId>,
    ) -> Result<HashMap<HgChangesetId, Result<Location<HgChangesetId>, MononokeError>>, MononokeError>
    {
        let all_hg_ids: Vec<_> = hg_ids
            .iter()
            .cloned()
            .chain(hg_master_heads.clone().into_iter())
            .collect();
        let hg_to_bonsai: HashMap<HgChangesetId, ChangesetId> = self
            .blob_repo()
            .get_hg_bonsai_mapping(self.ctx().clone(), all_hg_ids)
            .await?
            .into_iter()
            .collect();
        let master_heads = hg_master_heads
            .iter()
            .map(|master_id| {
                hg_to_bonsai.get(master_id).cloned().ok_or_else(|| {
                    MononokeError::InvalidRequest(format!(
                        "failed to find bonsai equivalent for client head {}",
                        master_id
                    ))
                })
            })
            .collect::<Result<Vec<_>, MononokeError>>()?;

        // We should treat hg_ids as being absolutely any hash. It is perfectly valid for the
        // server to have not encountered the hash that it was given to convert. Filter out the
        // hashes that we could not convert to bonsai.
        let cs_ids = hg_ids
            .iter()
            .filter_map(|hg_id| hg_to_bonsai.get(hg_id).cloned())
            .collect::<Vec<ChangesetId>>();

        let cs_to_blocations = self
            .repo()
            .many_changeset_ids_to_locations(master_heads, cs_ids)
            .await?;

        let bonsai_to_hg: HashMap<ChangesetId, HgChangesetId> = self
            .blob_repo()
            .get_hg_bonsai_mapping(
                self.ctx().clone(),
                cs_to_blocations
                    .iter()
                    .filter_map(|(_, result)| match result {
                        Ok(l) => Some(l.descendant),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            )
            .await?
            .into_iter()
            .map(|(hg_id, cs_id)| (cs_id, hg_id))
            .collect();
        let response = hg_ids
            .into_iter()
            .filter_map(|hg_id| hg_to_bonsai.get(&hg_id).map(|cs_id| (hg_id, cs_id)))
            .filter_map(|(hg_id, cs_id)| {
                cs_to_blocations
                    .get(cs_id)
                    .map(|cs_result| (hg_id, cs_result.clone()))
            })
            .map(|(hg_id, cs_result)| {
                let cs_result = match cs_result {
                    Ok(cs_location) => cs_location.try_map_descendant(|descendant| {
                        bonsai_to_hg.get(&descendant).cloned().ok_or_else(|| {
                            MononokeError::InvalidRequest(format!(
                                "failed to find hg equivalent for bonsai {}",
                                descendant
                            ))
                        })
                    }),
                    Err(e) => Err(e),
                };
                (hg_id, cs_result)
            })
            .collect::<HashMap<HgChangesetId, Result<Location<HgChangesetId>, MononokeError>>>();

        Ok(response)
    }

    pub async fn revlog_commit_data(
        &self,
        hg_cs_id: HgChangesetId,
    ) -> Result<Option<Bytes>, MononokeError> {
        let ctx = self.ctx();
        let blobstore = self.blob_repo().blobstore();
        let revlog_cs = RevlogChangeset::load(ctx, blobstore, hg_cs_id)
            .await
            .map_err(MononokeError::from)?;
        let revlog_cs = match revlog_cs {
            None => return Ok(None),
            Some(x) => x,
        };

        let mut buffer = Vec::new();
        revlog_cs
            .generate_for_hash_verification(&mut buffer)
            .map_err(MononokeError::from)?;
        Ok(Some(buffer.into()))
    }

    pub async fn segmented_changelog_clone_data(
        &self,
    ) -> Result<CloneData<HgChangesetId>, MononokeError> {
        let m_clone_data = self.repo().segmented_changelog_clone_data().await?;
        self.convert_clone_data(m_clone_data).await
    }

    pub async fn segmented_changelog_disabled(&self) -> Result<bool, MononokeError> {
        self.repo().segmented_changelog_disabled().await
    }

    pub async fn segmented_changelog_pull_fast_forward_master(
        &self,
        old_master: HgChangesetId,
        new_master: HgChangesetId,
    ) -> Result<CloneData<HgChangesetId>, MononokeError> {
        let hg_to_bonsai: HashMap<HgChangesetId, ChangesetId> = self
            .blob_repo()
            .get_hg_bonsai_mapping(self.ctx().clone(), vec![old_master, new_master])
            .await?
            .into_iter()
            .collect();
        let old_master = *hg_to_bonsai
            .get(&old_master)
            .ok_or_else(|| format_err!("Failed to convert old_master {} to bonsai", old_master))?;
        let new_master = *hg_to_bonsai
            .get(&new_master)
            .ok_or_else(|| format_err!("Failed to convert new_master {} to bonsai", new_master))?;
        let m_clone_data = self
            .repo()
            .segmented_changelog_pull_fast_forward_master(old_master, new_master)
            .await?;
        self.convert_clone_data(m_clone_data).await
    }

    async fn convert_clone_data(
        &self,
        m_clone_data: CloneData<ChangesetId>,
    ) -> Result<CloneData<HgChangesetId>, MononokeError> {
        const CHUNK_SIZE: usize = 1000;
        let idmap_list = m_clone_data.idmap.into_iter().collect::<Vec<_>>();
        let mut hg_idmap = BTreeMap::new();
        for chunk in idmap_list.chunks(CHUNK_SIZE) {
            let csids = chunk.iter().map(|(_, csid)| *csid).collect::<Vec<_>>();
            let mapping = self
                .blob_repo()
                .get_hg_bonsai_mapping(self.ctx().clone(), csids)
                .await
                .context("error fetching hg bonsai mapping")?
                .into_iter()
                .map(|(hgid, csid)| (csid, hgid))
                .collect::<HashMap<_, _>>();
            for (v, csid) in chunk {
                let hgid = mapping.get(&csid).ok_or_else(|| {
                    MononokeError::from(format_err!(
                        "failed to find bonsai '{}' mapping to hg",
                        csid
                    ))
                })?;
                hg_idmap.insert(*v, *hgid);
            }
        }
        let hg_clone_data = CloneData {
            flat_segments: m_clone_data.flat_segments,
            idmap: hg_idmap,
        };
        Ok(hg_clone_data)
    }

    /// resolve a bookmark name to an Hg Changeset
    pub async fn resolve_bookmark(
        &self,
        bookmark: impl AsRef<str>,
        freshness: Freshness,
    ) -> Result<Option<HgChangesetId>, MononokeError> {
        match self.repo.resolve_bookmark(bookmark, freshness).await? {
            Some(c) => c.hg_id().await,
            None => Ok(None),
        }
    }

    /// Return (at most 10) HgChangesetIds in the range described by the low and high parameters.
    pub async fn get_hg_in_range(
        &self,
        low: HgChangesetId,
        high: HgChangesetId,
    ) -> Result<Vec<HgChangesetId>, MononokeError> {
        const LIMIT: usize = 10;
        let repo_id = self.repo().repoid();
        let bonsai_hg_mapping = self.blob_repo().bonsai_hg_mapping();
        bonsai_hg_mapping
            .get_hg_in_range(self.ctx(), repo_id, low, high, LIMIT)
            .await
            .map_err(|e| e.into())
    }

    /// return a mapping of commits to their parents that are in the segment of
    /// of the commit graph bounded by common and heads.
    pub async fn get_graph_mapping(
        &self,
        common: Vec<HgChangesetId>,
        heads: Vec<HgChangesetId>,
    ) -> Result<HashMap<HgChangesetId, Vec<HgChangesetId>>, MononokeError> {
        let ctx = self.ctx().clone();
        let blob_repo = self.blob_repo();
        let lca_hint: Arc<dyn LeastCommonAncestorsHint> = self.repo.skiplist_index().clone();
        let phases = blob_repo.phases();
        let common: HashSet<_> = common.into_iter().collect();
        // make sure filenodes are derived before sending
        let (_draft_commits, missing_commits) = try_join!(
            find_new_draft_commits_and_derive_filenodes_for_public_roots(
                &ctx, blob_repo, &common, &heads, phases
            ),
            find_commits_to_send(&ctx, blob_repo, &common, &heads, &lca_hint),
        )?;

        let cs_parent_mapping: HashMap<ChangesetId, Vec<ChangesetId>> =
            stream::iter(missing_commits.clone().into_iter())
                .map(move |cs_id| async move {
                    let parents = blob_repo
                        .get_changeset_parents_by_bonsai(self.ctx().clone(), cs_id)
                        .await?;
                    Ok::<_, Error>((cs_id, parents))
                })
                .buffered(100)
                .try_collect::<HashMap<_, _>>()
                .await?;

        let cs_ids = cs_parent_mapping
            .clone()
            .into_values()
            .flatten()
            .chain(missing_commits)
            .collect::<HashSet<_>>();
        let bonsai_hg_mapping = self
            .blob_repo()
            .get_hg_bonsai_mapping(
                self.ctx().clone(),
                cs_ids.into_iter().collect::<Vec<ChangesetId>>(),
            )
            .await
            .context("error fetching hg bonsai mapping")?
            .into_iter()
            .map(|(hg_id, cs_id)| (cs_id, hg_id))
            .collect::<HashMap<_, _>>();

        let mut hg_parent_mapping: HashMap<HgChangesetId, Vec<HgChangesetId>> = HashMap::new();
        let get_hg_id = |cs_id| {
            bonsai_hg_mapping
                .get(cs_id)
                .cloned()
                .with_context(|| format_err!("failed to find bonsai '{}' mapping to hg", cs_id))
        };

        for (cs_id, cs_parents) in cs_parent_mapping.iter() {
            let hg_id = get_hg_id(cs_id)?;
            let hg_parents = cs_parents
                .into_iter()
                .map(get_hg_id)
                .collect::<Result<Vec<HgChangesetId>, Error>>()
                .map_err(MononokeError::from)?;
            hg_parent_mapping.insert(hg_id, hg_parents);
        }
        Ok(hg_parent_mapping)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;
    use std::sync::Arc;

    use anyhow::Error;
    use blobstore::Loadable;
    use fbinit::FacebookInit;
    use mononoke_api::repo::Repo;
    use mononoke_types::ChangesetId;
    use tests_utils::CreateCommitContext;

    use crate::RepoContextHgExt;

    #[fbinit::test]
    async fn test_new_hg_context(fb: FacebookInit) -> Result<(), MononokeError> {
        let ctx = CoreContext::test_mock(fb);

        let blob_repo: BlobRepo = test_repo_factory::build_empty()?;
        let repo = Repo::new_test(ctx.clone(), blob_repo).await?;
        let repo_ctx = RepoContext::new(ctx, Arc::new(repo)).await?;

        let hg = repo_ctx.hg();
        assert_eq!(hg.repo().name(), "test");

        Ok(())
    }

    #[fbinit::test]
    async fn test_trees_under_path(fb: FacebookInit) -> Result<(), MononokeError> {
        let ctx = CoreContext::test_mock(fb);
        let blob_repo: BlobRepo = test_repo_factory::build_empty()?;

        // Create test stack; child commit modifies 2 directories.
        let commit_1 = CreateCommitContext::new_root(&ctx, &blob_repo)
            .add_file("dir1/a", "1")
            .add_file("dir2/b", "1")
            .add_file("dir3/c", "1")
            .commit()
            .await?;
        let commit_2 = CreateCommitContext::new(&ctx, &blob_repo, vec![commit_1])
            .add_file("dir1/a", "2")
            .add_file("dir3/a/b/c", "1")
            .commit()
            .await?;

        let root_mfid_1 = root_manifest_id(ctx.clone(), &blob_repo, commit_1).await?;
        let root_mfid_2 = root_manifest_id(ctx.clone(), &blob_repo, commit_2).await?;

        let repo = Repo::new_test(ctx.clone(), blob_repo).await?;
        let repo_ctx = RepoContext::new(ctx, Arc::new(repo)).await?;
        let hg = repo_ctx.hg();

        let trees = hg
            .trees_under_path(
                MononokePath::new(None),
                vec![root_mfid_2],
                vec![root_mfid_1],
                Some(2),
            )
            .try_collect::<Vec<_>>()
            .await?;

        let paths = trees
            .into_iter()
            .map(|(_, path)| format!("{}", path))
            .collect::<BTreeSet<_>>();
        let expected = vec!["", "dir3", "dir1", "dir3/a"]
            .into_iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();

        assert_eq!(paths, expected);

        Ok(())
    }

    /// Get the HgManifestId of the root tree manifest for the given commit.
    async fn root_manifest_id(
        ctx: CoreContext,
        blob_repo: &BlobRepo,
        csid: ChangesetId,
    ) -> Result<HgManifestId, Error> {
        let hg_cs_id = blob_repo
            .get_hg_from_bonsai_changeset(ctx.clone(), csid)
            .await?;
        let hg_cs = hg_cs_id.load(&ctx, &blob_repo.get_blobstore()).await?;
        Ok(hg_cs.manifestid())
    }
}
