/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use bytes::Bytes;
#[cfg(any(test, feature = "for-tests"))]
use quickcheck::Arbitrary;
use serde_derive::{Deserialize, Serialize};

use crate::{
    tree::{
        TreeAttributes, TreeChildDirectoryEntry, TreeChildEntry, TreeChildFileEntry, TreeEntry,
        TreeRequest,
    },
    wire::{
        is_default, ToApi, ToWire, WireDirectoryMetadata, WireEdenApiServerError, WireFileMetadata,
        WireKey, WireParents, WireToApiConversionError,
    },
    EdenApiServerError,
};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WireTreeEntry {
    #[serde(rename = "0", default, skip_serializing_if = "is_default")]
    key: Option<WireKey>,

    #[serde(rename = "1", default, skip_serializing_if = "is_default")]
    data: Option<Bytes>,

    #[serde(rename = "2", default, skip_serializing_if = "is_default")]
    parents: Option<WireParents>,

    #[serde(rename = "3", default, skip_serializing_if = "is_default")]
    children: Option<Vec<WireTreeChildEntry>>,

    #[serde(rename = "4", default, skip_serializing_if = "is_default")]
    error: Option<WireEdenApiServerError>,
}

impl ToWire for Result<TreeEntry, EdenApiServerError> {
    type Wire = WireTreeEntry;

    fn to_wire(self) -> Self::Wire {
        match self {
            Ok(t) => WireTreeEntry {
                key: Some(t.key.to_wire()),
                data: t.data,
                parents: t.parents.to_wire(),
                children: t.children.to_wire(),
                error: None,
            },
            Err(e) => WireTreeEntry {
                key: e.key.to_wire(),
                error: Some(e.err.to_wire()),
                ..Default::default()
            },
        }
    }
}

impl ToApi for WireTreeEntry {
    type Api = Result<TreeEntry, EdenApiServerError>;
    type Error = WireToApiConversionError;

    fn to_api(self) -> Result<Self::Api, Self::Error> {
        Ok(if let (key, Some(err)) = (self.key.clone(), self.error) {
            Err(EdenApiServerError {
                key: key.to_api()?,
                err: err.to_api()?,
            })
        } else {
            Ok(TreeEntry {
                key: self
                    .key
                    .to_api()?
                    .ok_or(WireToApiConversionError::CannotPopulateRequiredField("key"))?,
                data: self.data,
                parents: self.parents.to_api()?,
                children: self.children.to_api()?,
            })
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WireTreeChildEntry {
    #[serde(rename = "0", default, skip_serializing_if = "is_default")]
    key: Option<WireKey>,

    #[serde(rename = "3", default, skip_serializing_if = "is_default")]
    file_metadata: Option<WireFileMetadata>,

    #[serde(rename = "4", default, skip_serializing_if = "is_default")]
    directory_metadata: Option<WireDirectoryMetadata>,

    #[serde(rename = "4", default, skip_serializing_if = "is_default")]
    error: Option<WireEdenApiServerError>,
}

impl ToWire for Result<TreeChildEntry, EdenApiServerError> {
    type Wire = WireTreeChildEntry;

    fn to_wire(self) -> Self::Wire {
        match self {
            Ok(TreeChildEntry::File(t)) => WireTreeChildEntry {
                key: Some(t.key.to_wire()),
                file_metadata: t.file_metadata.to_wire(),
                directory_metadata: None,
                error: None,
            },
            Ok(TreeChildEntry::Directory(t)) => WireTreeChildEntry {
                key: Some(t.key.to_wire()),
                file_metadata: None,
                directory_metadata: t.directory_metadata.to_wire(),
                error: None,
            },
            Err(e) => WireTreeChildEntry {
                key: e.key.to_wire(),
                error: Some(e.err.to_wire()),
                ..Default::default()
            },
        }
    }
}

impl ToApi for WireTreeChildEntry {
    type Api = Result<TreeChildEntry, EdenApiServerError>;
    type Error = WireToApiConversionError;

    fn to_api(self) -> Result<Self::Api, Self::Error> {
        Ok(if let (key, Some(err)) = (self.key.clone(), self.error) {
            Err(EdenApiServerError {
                key: key.to_api()?,
                err: err.to_api()?,
            })
        } else {
            Ok(
                if let (key, Some(file_metadata)) = (self.key.clone(), self.file_metadata) {
                    TreeChildEntry::File(TreeChildFileEntry {
                        key: key
                            .to_api()?
                            .ok_or(WireToApiConversionError::CannotPopulateRequiredField("key"))?,
                        file_metadata: Some(file_metadata.to_api()?),
                    })
                } else {
                    TreeChildEntry::Directory(TreeChildDirectoryEntry {
                        key: self
                            .key
                            .to_api()?
                            .ok_or(WireToApiConversionError::CannotPopulateRequiredField("key"))?,
                        directory_metadata: self.directory_metadata.to_api()?,
                    })
                },
            )
        })
    }
}

#[derive(Clone, Default, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct WireTreeKeysQuery {
    pub keys: Vec<WireKey>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum WireTreeQuery {
    #[serde(rename = "1")]
    ByKeys(WireTreeKeysQuery),

    #[serde(other, rename = "0")]
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct WireTreeAttributesRequest {
    #[serde(rename = "0", default, skip_serializing_if = "is_default")]
    with_data: bool,

    #[serde(rename = "1", default, skip_serializing_if = "is_default")]
    with_parents: bool,

    #[serde(rename = "4", default, skip_serializing_if = "is_default")]
    with_child_metadata: bool,
}

impl ToWire for TreeAttributes {
    type Wire = WireTreeAttributesRequest;

    fn to_wire(self) -> Self::Wire {
        WireTreeAttributesRequest {
            with_data: self.manifest_blob,
            with_parents: self.parents,
            with_child_metadata: self.child_metadata,
        }
    }
}

impl ToApi for WireTreeAttributesRequest {
    type Api = TreeAttributes;
    type Error = WireToApiConversionError;

    fn to_api(self) -> Result<Self::Api, Self::Error> {
        Ok(TreeAttributes {
            child_metadata: self.with_child_metadata,
            parents: self.with_parents,
            manifest_blob: self.with_data,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct WireTreeRequest {
    #[serde(rename = "0", default, skip_serializing_if = "is_default")]
    query: Option<WireTreeQuery>,

    #[serde(rename = "1", default, skip_serializing_if = "is_default")]
    attributes: Option<WireTreeAttributesRequest>,
}

impl ToWire for TreeRequest {
    type Wire = WireTreeRequest;

    fn to_wire(self) -> Self::Wire {
        WireTreeRequest {
            query: Some(WireTreeQuery::ByKeys(WireTreeKeysQuery {
                keys: self.keys.to_wire(),
            })),

            attributes: Some(self.attributes.to_wire()),
        }
    }
}

impl ToApi for WireTreeRequest {
    type Api = TreeRequest;
    type Error = WireToApiConversionError;

    fn to_api(self) -> Result<Self::Api, Self::Error> {
        Ok(TreeRequest {
            keys: match self.query {
                Some(WireTreeQuery::ByKeys(kq)) => kq.keys.to_api()?,
                Some(_) => {
                    return Err(WireToApiConversionError::UnrecognizedEnumVariant(
                        "WireTreeQuery",
                    ));
                }
                None => {
                    return Err(WireToApiConversionError::CannotPopulateRequiredField(
                        "keys",
                    ));
                }
            },
            attributes: self.attributes.to_api()?.unwrap_or_default(),
        })
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for WireTreeEntry {
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        let bytes: Option<Vec<u8>> = Arbitrary::arbitrary(g);
        Self {
            key: Arbitrary::arbitrary(g),
            data: bytes.map(|b| Bytes::from(b)),
            parents: Arbitrary::arbitrary(g),
            // Not recursing here because it causes Quickcheck to overflow the stack
            children: None,
            // TODO
            error: None,
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for WireTreeAttributesRequest {
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        Self {
            with_data: Arbitrary::arbitrary(g),
            with_parents: Arbitrary::arbitrary(g),
            with_child_metadata: Arbitrary::arbitrary(g),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for WireTreeKeysQuery {
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        Self {
            keys: Arbitrary::arbitrary(g),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for WireTreeQuery {
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        use rand::Rng;
        let variant = g.gen_range(0, 2);
        match variant {
            0 => WireTreeQuery::Other,
            1 => WireTreeQuery::ByKeys(Arbitrary::arbitrary(g)),
            _ => unreachable!(),
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for WireTreeRequest {
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        Self {
            query: Arbitrary::arbitrary(g),
            attributes: Arbitrary::arbitrary(g),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::wire::tests::{check_serialize_roundtrip, check_wire_roundtrip};

    use quickcheck::quickcheck;

    quickcheck! {
        // Wire serialize roundtrips
        fn test_keys_query_roundtrip_serialize(v: WireTreeKeysQuery) -> bool {
            check_serialize_roundtrip(v)
        }

        fn test_query_roundtrip_serialize(v: WireTreeQuery) -> bool {
            check_serialize_roundtrip(v)
        }

        fn test_attrs_roundtrip_serialize(v: WireTreeAttributesRequest) -> bool {
            check_serialize_roundtrip(v)
        }

        fn test_request_roundtrip_serialize(v: WireTreeRequest) -> bool {
            check_serialize_roundtrip(v)
        }

        fn test_entry_roundtrip_serialize(v: WireTreeEntry) -> bool {
            check_serialize_roundtrip(v)
        }

        // API-Wire roundtrips

        fn test_request_roundtrip_wire(v: TreeRequest) -> bool {
            check_wire_roundtrip(v)
        }

        fn test_entry_roundtrip_wire(v: Result<TreeEntry, EdenApiServerError>) -> bool {
            check_wire_roundtrip(v)
        }
    }
}
