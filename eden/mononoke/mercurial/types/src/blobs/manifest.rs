/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! Root manifest, tree nodes

use super::errors::ErrorKind;
use crate::{
    nodehash::{HgNodeHash, NULL_HASH},
    FileType, HgBlob, HgEntryId, HgFileNodeId, HgManifestEnvelope, HgManifestId, HgParents,
    MPathElement, Type,
};
use anyhow::{bail, ensure, Context, Error, Result};
use blobstore::{Blobstore, Loadable, LoadableError};
use context::CoreContext;
use failure_ext::FutureFailureErrorExt;
use futures::{
    compat::Future01CompatExt,
    future::{BoxFuture, FutureExt, TryFutureExt},
};
use futures_ext::{BoxFuture as BoxFuture01, FutureExt as _};
use futures_old::future::{self, Future};
use manifest::{Entry, Manifest};
use sorted_vector_map::SortedVectorMap;
use std::str;

#[derive(Debug, Eq, PartialEq)]
pub struct ManifestContent {
    pub files: SortedVectorMap<MPathElement, HgEntryId>,
}

impl ManifestContent {
    pub fn new_empty() -> Self {
        Self {
            files: SortedVectorMap::new(),
        }
    }

    // Each manifest revision contains a list of the file revisions in each changeset, in the form:
    //
    // <filename>\0<hex file revision id>[<flags>]\n
    //
    // Source: mercurial/parsers.c:parse_manifest()

    //
    // NB: filenames are sequences of non-zero bytes, not strings
    fn parse_impl(data: &[u8]) -> Result<SortedVectorMap<MPathElement, HgEntryId>> {
        let lines = data.split(|b| *b == b'\n');
        let mut files = match lines.size_hint() {
            // Split returns it count in the high size hint
            (_, Some(high)) => SortedVectorMap::with_capacity(high),
            (_, None) => SortedVectorMap::new(),
        };

        for line in lines {
            if line.is_empty() {
                break;
            }

            let (name, rest) = match find(line, &0) {
                None => bail!("Malformed entry: no \\0"),
                Some(nil) => {
                    let (name, rest) = line.split_at(nil);
                    if let Some((_, hash)) = rest.split_first() {
                        (name, hash)
                    } else {
                        bail!("Malformed entry: no hash");
                    }
                }
            };

            let path = MPathElement::new(name.to_vec()).context("invalid path in manifest")?;
            let entry_id = parse_hg_entry(rest)?;

            files.insert(path, entry_id);
        }

        Ok(files)
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        Ok(Self {
            files: Self::parse_impl(data)?,
        })
    }
}

pub fn fetch_raw_manifest_bytes<B: Blobstore>(
    ctx: CoreContext,
    blobstore: &B,
    manifest_id: HgManifestId,
) -> BoxFuture01<HgBlob, Error> {
    fetch_manifest_envelope(ctx, blobstore, manifest_id)
        .map(move |envelope| {
            let envelope = envelope.into_mut();
            HgBlob::from(envelope.contents)
        })
        .from_err()
        .boxify()
}

pub fn fetch_manifest_envelope<B: Blobstore>(
    ctx: CoreContext,
    blobstore: &B,
    manifest_id: HgManifestId,
) -> impl Future<Item = HgManifestEnvelope, Error = Error> {
    fetch_manifest_envelope_opt(ctx, blobstore, manifest_id)
        .and_then(move |envelope| {
            let envelope = envelope.ok_or(ErrorKind::HgContentMissing(
                manifest_id.into_nodehash(),
                Type::Tree,
            ))?;
            Ok(envelope)
        })
        .from_err()
}

/// Like `fetch_manifest_envelope`, but returns None if the manifest wasn't found.
pub fn fetch_manifest_envelope_opt<B: Blobstore>(
    ctx: CoreContext,
    blobstore: &B,
    node_id: HgManifestId,
) -> impl Future<Item = Option<HgManifestEnvelope>, Error = Error> {
    let blobstore_key = node_id.blobstore_key();
    blobstore
        .get(ctx, blobstore_key.clone())
        .compat()
        .context("While fetching manifest envelope blob")
        .map_err(Error::from)
        .and_then(move |bytes| {
            let blobstore_bytes = match bytes {
                Some(bytes) => bytes,
                None => return Ok(None),
            };
            let envelope = HgManifestEnvelope::from_blob(blobstore_bytes.into())?;
            if node_id.into_nodehash() != envelope.node_id() {
                bail!(
                    "Manifest ID mismatch (requested: {}, got: {})",
                    node_id,
                    envelope.node_id()
                );
            }
            Ok(Some(envelope))
        })
        .context(ErrorKind::ManifestDeserializeFailed(blobstore_key))
        .from_err()
}

#[derive(Debug)]
pub struct BlobManifest {
    node_id: HgNodeHash,
    p1: Option<HgNodeHash>,
    p2: Option<HgNodeHash>,
    // See the documentation in mercurial_types/if/mercurial.thrift for why this exists.
    computed_node_id: HgNodeHash,
    content: ManifestContent,
}

impl BlobManifest {
    pub fn load<B: Blobstore>(
        ctx: CoreContext,
        blobstore: &B,
        manifestid: HgManifestId,
    ) -> impl Future<Item = Option<Self>, Error = Error> {
        if manifestid.clone().into_nodehash() == NULL_HASH {
            future::ok(Some(BlobManifest {
                node_id: NULL_HASH,
                p1: None,
                p2: None,
                computed_node_id: NULL_HASH,
                content: ManifestContent::new_empty(),
            }))
            .left_future()
        } else {
            fetch_manifest_envelope_opt(ctx, blobstore, manifestid)
                .and_then({
                    move |envelope| match envelope {
                        Some(envelope) => Ok(Some(Self::parse(envelope)?)),
                        None => Ok(None),
                    }
                })
                .context(format!(
                    "When loading manifest {} from blobstore",
                    manifestid
                ))
                .from_err()
                .right_future()
        }
    }

    pub fn parse(envelope: HgManifestEnvelope) -> Result<Self> {
        let envelope = envelope.into_mut();
        let content = ManifestContent::parse(envelope.contents.as_ref()).with_context(|| {
            format!(
                "while parsing contents for manifest ID {}",
                envelope.node_id
            )
        })?;
        Ok(BlobManifest {
            node_id: envelope.node_id,
            p1: envelope.p1,
            p2: envelope.p2,
            computed_node_id: envelope.computed_node_id,
            content,
        })
    }

    #[inline]
    pub fn node_id(&self) -> HgNodeHash {
        self.node_id
    }

    #[inline]
    pub fn p1(&self) -> Option<HgNodeHash> {
        self.p1
    }

    #[inline]
    pub fn p2(&self) -> Option<HgNodeHash> {
        self.p2
    }

    #[inline]
    pub fn hg_parents(&self) -> HgParents {
        HgParents::new(self.p1, self.p2)
    }

    #[inline]
    pub fn computed_node_id(&self) -> HgNodeHash {
        self.computed_node_id
    }
}

impl Loadable for HgManifestId {
    type Value = BlobManifest;

    fn load<B: Blobstore + Clone>(
        &self,
        ctx: CoreContext,
        blobstore: &B,
    ) -> BoxFuture<'static, Result<Self::Value, LoadableError>> {
        let id = *self;
        let value = BlobManifest::load(ctx, blobstore, id).compat();

        async move {
            value
                .await?
                .ok_or_else(|| LoadableError::Missing(id.blobstore_key()))
        }
        .boxed()
    }
}

impl Manifest for BlobManifest {
    type TreeId = HgManifestId;
    type LeafId = (FileType, HgFileNodeId);

    fn lookup(&self, name: &MPathElement) -> Option<Entry<Self::TreeId, Self::LeafId>> {
        self.content.files.get(name).copied().map(Entry::from)
    }

    fn list(&self) -> Box<dyn Iterator<Item = (MPathElement, Entry<Self::TreeId, Self::LeafId>)>> {
        let iter = self
            .content
            .files
            .clone()
            .into_iter()
            .map(|(name, hg_entry_id)| (name, Entry::from(hg_entry_id)));
        Box::new(iter)
    }
}

fn parse_hg_entry(data: &[u8]) -> Result<HgEntryId> {
    ensure!(data.len() >= 40, "hash too small: {:?}", data);

    let (hash, flags) = data.split_at(40);
    let hash = str::from_utf8(hash)
        .map_err(|err| Error::from(err))
        .and_then(|hash| hash.parse::<HgNodeHash>())
        .with_context(|| format!("malformed hash: {:?}", hash))?;
    ensure!(flags.len() <= 1, "More than 1 flag: {:?}", flags);

    let hg_entry_id = if flags.is_empty() {
        HgEntryId::File(FileType::Regular, HgFileNodeId::new(hash))
    } else {
        match flags[0] {
            b'l' => HgEntryId::File(FileType::Symlink, HgFileNodeId::new(hash)),
            b'x' => HgEntryId::File(FileType::Executable, HgFileNodeId::new(hash)),
            b't' => HgEntryId::Manifest(HgManifestId::new(hash)),
            unk => bail!("Unknown flag {}", unk),
        }
    };

    Ok(hg_entry_id)
}

fn find<T>(haystack: &[T], needle: &T) -> Option<usize>
where
    T: PartialEq,
{
    haystack.iter().position(|e| e == needle)
}
