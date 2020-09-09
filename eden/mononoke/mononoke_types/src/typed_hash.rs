/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::fmt::{self, Display};
use std::{result, str::FromStr};

use abomonation_derive::Abomonation;
use anyhow::{bail, Error, Result};
use ascii::{AsciiStr, AsciiString};
use blobstore::{Blobstore, Loadable, LoadableError, Storable};
use context::CoreContext;
use futures::future::{BoxFuture, FutureExt};
use quickcheck::{empty_shrinker, Arbitrary, Gen};

use crate::{
    blob::{Blob, BlobstoreValue},
    bonsai_changeset::BonsaiChangeset,
    content_chunk::ContentChunk,
    content_metadata::ContentMetadata,
    deleted_files_manifest::DeletedManifest,
    errors::ErrorKind,
    fastlog_batch::FastlogBatch,
    file_contents::FileContents,
    fsnode::Fsnode,
    hash::{Blake2, Blake2Prefix, Context},
    rawbundle2::RawBundle2,
    thrift,
    unode::{FileUnode, ManifestUnode},
};

// There is no NULL_HASH for typed hashes. Any places that need a null hash should use an
// Option type, or perhaps a list as desired.

/// An identifier used throughout Mononoke.
pub trait MononokeId: Copy + Sync + Send + 'static {
    /// Blobstore value type associated with given MononokeId type
    type Value: BlobstoreValue<Key = Self>;

    /// Return a key suitable for blobstore use.
    fn blobstore_key(&self) -> String;

    /// Return a prefix before hash used in blobstore
    fn blobstore_key_prefix() -> String;

    /// Return a stable hash fingerprint that can be used for sampling
    fn sampling_fingerprint(&self) -> u64;
}

/// An identifier for a changeset in Mononoke.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash, Abomonation)]
pub struct ChangesetId(Blake2);

/// An identifier for a changeset hash prefix in Mononoke.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash, Abomonation)]
pub struct ChangesetIdPrefix(Blake2Prefix);

/// The type for resolving changesets by prefix of the hash
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ChangesetIdsResolvedFromPrefix {
    /// Found single changeset
    Single(ChangesetId),
    /// Found several changesets within the limit provided
    Multiple(Vec<ChangesetId>),
    /// Found too many changesets exceeding the limit provided
    TooMany(Vec<ChangesetId>),
    /// Changeset was not found
    NoMatch,
}

/// An identifier for file contents in Mononoke.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct ContentId(Blake2);

/// An identifier for a chunk of a file's contents.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct ContentChunkId(Blake2);

/// An identifier for mapping from a ContentId to various aliases for that content
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct ContentMetadataId(Blake2);

/// An identifier for raw bundle2 contents in Mononoke
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct RawBundle2Id(Blake2);

/// An identifier for a file unode
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct FileUnodeId(Blake2);

/// An identifier for a manifest unode
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct ManifestUnodeId(Blake2);

/// An identifier for a deleted files manifest
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct DeletedManifestId(Blake2);

/// An identifier for an fsnode
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct FsnodeId(Blake2);

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct FastlogBatchId(Blake2);

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct BlameId(Blake2);

struct Blake2HexVisitor;

impl<'de> serde::de::Visitor<'de> for Blake2HexVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("64 hex digits")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(value.to_string())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(value)
    }
}

/// Implementations of typed hashes.
macro_rules! impl_typed_hash_no_context {
    {
        hash_type => $typed: ident,
        value_type => $value_type: ident,
    } => {
        impl $typed {
            pub const fn new(blake2: Blake2) -> Self {
                $typed(blake2)
            }

            // (this is public because downstream code wants to be able to deserialize these nodes)
            pub fn from_thrift(h: thrift::$typed) -> Result<Self> {
                // This assumes that a null hash is never serialized. This should always be the
                // case.
                match h.0 {
                    thrift::IdType::Blake2(blake2) => Ok($typed(Blake2::from_thrift(blake2)?)),
                    thrift::IdType::UnknownField(x) => bail!(ErrorKind::InvalidThrift(
                        stringify!($typed).into(),
                        format!("unknown id type field: {}", x)
                    )),
                }
            }

            #[cfg(test)]
            pub(crate) fn from_byte_array(arr: [u8; 32]) -> Self {
                $typed(Blake2::from_byte_array(arr))
            }

            #[inline]
            pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Result<Self> {
                Blake2::from_bytes(bytes).map(Self::new)
            }

            #[inline]
            pub fn from_str(s: &str) -> Result<Self> {
                Blake2::from_str(s).map(Self::new)
            }

            #[inline]
            pub fn from_ascii_str(s: &AsciiStr) -> Result<Self> {
                Blake2::from_ascii_str(s).map(Self::new)
            }

            pub fn blake2(&self) -> &Blake2 {
                &self.0
            }

            #[inline]
            pub fn to_hex(&self) -> AsciiString {
                self.0.to_hex()
            }

            // (this is public because downstream code wants to be able to serialize these nodes)
            pub fn into_thrift(self) -> thrift::$typed {
                thrift::$typed(thrift::IdType::Blake2(self.0.into_thrift()))
            }
        }

        impl From<Blake2> for $typed {
            fn from(h: Blake2) -> $typed {
                $typed::new(h)
            }
        }

        impl<'a> From<&'a Blake2> for $typed {
            fn from(h: &'a Blake2) -> $typed {
                $typed::new(*h)
            }
        }

        impl AsRef<[u8]> for $typed {
            fn as_ref(&self) -> &[u8] {
                self.0.as_ref()
            }
        }

        impl Display for $typed {
            fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
                self.0.fmt(fmt)
            }
        }

        impl Arbitrary for $typed {
            fn arbitrary<G: Gen>(g: &mut G) -> Self {
                $typed(Blake2::arbitrary(g))
            }

            fn shrink(&self) -> Box<dyn Iterator<Item = Self>> {
                empty_shrinker()
            }
        }

        impl serde::Serialize for $typed {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.to_hex().as_str())
            }
        }

        impl<'de> serde::de::Deserialize<'de> for $typed {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::de::Deserializer<'de>,
            {
                let hex = deserializer.deserialize_string(Blake2HexVisitor)?;
                match Blake2::from_str(hex.as_str()) {
                    Ok(blake2) => Ok($typed(blake2)),
                    Err(error) => Err(serde::de::Error::custom(error)),
                }
            }
        }

    }
}

macro_rules! impl_typed_hash_loadable_storable {
    {
        hash_type => $typed: ident,
    } => {
        impl Loadable for $typed
        {
            type Value = <$typed as MononokeId>::Value;

            fn load<B: Blobstore + Clone>(
                &self,
                ctx: CoreContext,
                blobstore: &B,
            ) -> BoxFuture<'static, Result<Self::Value, LoadableError>> {
                let id = *self;
                let blobstore_key = id.blobstore_key();
                let get = blobstore
                    .get(ctx, blobstore_key.clone());

                async move {
                    let bytes = get.await?.ok_or(LoadableError::Missing(blobstore_key))?;
                    let blob: Blob<$typed> = Blob::new(id, bytes.into_raw_bytes());
                    <$typed as MononokeId>::Value::from_blob(blob).map_err(LoadableError::Error)
                }
                .boxed()
            }
        }

        impl Storable for Blob<$typed>
        {
            type Key = $typed;

            fn store<B: Blobstore + Clone>(
                self,
                ctx: CoreContext,
                blobstore: &B,
            ) -> BoxFuture<'static, Result<Self::Key, Error>> {
                let id = *self.id();
                let bytes = self.into();
                let put = blobstore.put(ctx, id.blobstore_key(), bytes);

                async move {
                    put.await?;
                    Ok(id)
                }.boxed()
            }
        }
    }
}

macro_rules! impl_typed_hash {
    {
        hash_type => $typed: ident,
        value_type => $value_type: ident,
        context_type => $typed_context: ident,
        context_key => $key: expr,
    } => {
        impl_typed_hash_no_context! {
            hash_type => $typed,
            value_type => $value_type,
        }

        impl_typed_hash_loadable_storable! {
            hash_type => $typed,
        }

        /// Context for incrementally computing a hash.
        #[derive(Clone)]
        pub struct $typed_context(Context);

        impl $typed_context {
            /// Construct a context.
            #[inline]
            pub fn new() -> Self {
                $typed_context(Context::new($key.as_bytes()))
            }

            #[inline]
            pub fn update<T>(&mut self, data: T)
            where
                T: AsRef<[u8]>,
            {
                self.0.update(data)
            }

            #[inline]
            pub fn finish(self) -> $typed {
                $typed(self.0.finish())
            }
        }

        impl MononokeId for $typed {
            type Value = $value_type;

            #[inline]
            fn blobstore_key(&self) -> String {
                format!(concat!($key, ".blake2.{}"), self.0)
            }

            #[inline]
            fn blobstore_key_prefix() -> String {
                concat!($key, ".blake2.").to_string()
            }

            #[inline]
            fn sampling_fingerprint(&self) -> u64 {
                self.0.sampling_fingerprint()
            }
        }

    }
}

impl_typed_hash! {
    hash_type => ChangesetId,
    value_type => BonsaiChangeset,
    context_type => ChangesetIdContext,
    context_key => "changeset",
}

impl_typed_hash! {
    hash_type => ContentId,
    value_type => FileContents,
    context_type => ContentIdContext,
    context_key => "content",
}

impl_typed_hash! {
    hash_type => ContentChunkId,
    value_type => ContentChunk,
    context_type => ContentChunkIdContext,
    context_key => "chunk",
}

impl_typed_hash! {
    hash_type => RawBundle2Id,
    value_type => RawBundle2,
    context_type => RawBundle2IdContext,
    context_key => "rawbundle2",
}

impl_typed_hash! {
    hash_type => FileUnodeId,
    value_type => FileUnode,
    context_type => FileUnodeIdContext,
    context_key => "fileunode",
}

impl_typed_hash! {
    hash_type => ManifestUnodeId,
    value_type => ManifestUnode,
    context_type => ManifestUnodeIdContext,
    context_key => "manifestunode",
}

impl_typed_hash! {
    hash_type => DeletedManifestId,
    value_type => DeletedManifest,
    context_type => DeletedManifestContext,
    context_key => "deletedmanifest",
}

impl_typed_hash! {
    hash_type => FsnodeId,
    value_type => Fsnode,
    context_type => FsnodeIdContext,
    context_key => "fsnode",
}

impl_typed_hash_no_context! {
    hash_type => ContentMetadataId,
    value_type => ContentMetadata,
}

impl_typed_hash_loadable_storable! {
    hash_type => ContentMetadataId,
}

impl_typed_hash! {
    hash_type => FastlogBatchId,
    value_type => FastlogBatch,
    context_type => FastlogBatchIdContext,
    context_key => "fastlogbatch",
}

impl ContentMetadataId {
    const PREFIX: &'static str = "content_metadata.blake2";
}

impl From<ContentId> for ContentMetadataId {
    fn from(content: ContentId) -> Self {
        Self { 0: content.0 }
    }
}

impl MononokeId for ContentMetadataId {
    type Value = ContentMetadata;

    #[inline]
    fn blobstore_key(&self) -> String {
        format!("{}.{}", Self::PREFIX, self.0)
    }

    #[inline]
    fn blobstore_key_prefix() -> String {
        Self::PREFIX.to_string()
    }

    #[inline]
    fn sampling_fingerprint(&self) -> u64 {
        self.0.sampling_fingerprint()
    }
}

impl ChangesetIdPrefix {
    pub const fn new(blake2prefix: Blake2Prefix) -> Self {
        ChangesetIdPrefix(blake2prefix)
    }

    pub fn from_bytes<B: AsRef<[u8]> + ?Sized>(bytes: &B) -> Result<Self> {
        Blake2Prefix::from_bytes(bytes).map(Self::new)
    }

    #[inline]
    pub fn min_as_ref(&self) -> &[u8] {
        self.0.min_as_ref()
    }

    #[inline]
    pub fn max_as_ref(&self) -> &[u8] {
        self.0.max_as_ref()
    }

    #[inline]
    pub fn into_changeset_id(self) -> Option<ChangesetId> {
        self.0.into_blake2().map(ChangesetId)
    }
}

impl FromStr for ChangesetIdPrefix {
    type Err = <Blake2Prefix as FromStr>::Err;
    fn from_str(s: &str) -> result::Result<ChangesetIdPrefix, Self::Err> {
        Blake2Prefix::from_str(s).map(ChangesetIdPrefix)
    }
}

impl Display for ChangesetIdPrefix {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(fmt)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use quickcheck::quickcheck;

    quickcheck! {
        fn changesetid_thrift_roundtrip(h: ChangesetId) -> bool {
            let v = h.into_thrift();
            let sh = ChangesetId::from_thrift(v)
                .expect("converting a valid Thrift structure should always work");
            h == sh
        }

        fn contentid_thrift_roundtrip(h: ContentId) -> bool {
            let v = h.into_thrift();
            let sh = ContentId::from_thrift(v)
                .expect("converting a valid Thrift structure should always work");
            h == sh
        }
    }

    #[test]
    fn blobstore_key() {
        // These IDs are persistent, and this test is really to make sure that they don't change
        // accidentally.
        let id = ChangesetId::new(Blake2::from_byte_array([1; 32]));
        assert_eq!(id.blobstore_key(), format!("changeset.blake2.{}", id));

        let id = ContentId::new(Blake2::from_byte_array([1; 32]));
        assert_eq!(id.blobstore_key(), format!("content.blake2.{}", id));

        let id = ContentChunkId::from_byte_array([1; 32]);
        assert_eq!(id.blobstore_key(), format!("chunk.blake2.{}", id));

        let id = RawBundle2Id::from_byte_array([1; 32]);
        assert_eq!(id.blobstore_key(), format!("rawbundle2.blake2.{}", id));

        let id = FileUnodeId::from_byte_array([1; 32]);
        assert_eq!(id.blobstore_key(), format!("fileunode.blake2.{}", id));

        let id = ManifestUnodeId::from_byte_array([1; 32]);
        assert_eq!(id.blobstore_key(), format!("manifestunode.blake2.{}", id));

        let id = DeletedManifestId::from_byte_array([1; 32]);
        assert_eq!(id.blobstore_key(), format!("deletedmanifest.blake2.{}", id));

        let id = FsnodeId::from_byte_array([1; 32]);
        assert_eq!(id.blobstore_key(), format!("fsnode.blake2.{}", id));

        let id = ContentMetadataId::from_byte_array([1; 32]);
        assert_eq!(
            id.blobstore_key(),
            format!("content_metadata.blake2.{}", id)
        );

        let id = FastlogBatchId::from_byte_array([1; 32]);
        assert_eq!(id.blobstore_key(), format!("fastlogbatch.blake2.{}", id));
    }

    #[test]
    fn test_serialize_deserialize() {
        let id = ChangesetId::new(Blake2::from_byte_array([1; 32]));
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);

        let id = ContentId::new(Blake2::from_byte_array([1; 32]));
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);

        let id = ContentChunkId::from_byte_array([1; 32]);
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);

        let id = RawBundle2Id::from_byte_array([1; 32]);
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);

        let id = FileUnodeId::from_byte_array([1; 32]);
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);

        let id = ManifestUnodeId::from_byte_array([1; 32]);
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);

        let id = DeletedManifestId::from_byte_array([1; 32]);
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);

        let id = FsnodeId::from_byte_array([1; 32]);
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);

        let id = ContentMetadataId::from_byte_array([1; 32]);
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);

        let id = FastlogBatchId::from_byte_array([1; 32]);
        let serialized = serde_json::to_string(&id).unwrap();
        let deserialized = serde_json::from_str(&serialized).unwrap();
        assert_eq!(id, deserialized);
    }
}
