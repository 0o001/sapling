/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! Utilities for parsing requests from JSON.
//!
//! This module provides the ability to create various EdenAPI request
//! types from human-editable JSON. This is primarily useful for testing
//! debugging, since it provides a convenient way for a developer to
//! create ad-hoc requests. Some of the EdenAPI testing tools accept
//! requests in this format.
//!
//! Note that even though the request structs implement `Deserialize`,
//! we are explicitly not using their `Deserialize` implementations
//! since the format used here does not correspond exactly to the actual
//! representation used in production. (For examples, hashes are
//! represented as hexadecimal strings rather than as byte arrays.)

use std::convert::TryFrom;
use std::str::FromStr;

use anyhow::{ensure, Context, Result};
use serde_json::{json, Map, Value};

use types::{HgId, Key, RepoPathBuf};

use crate::commit::{CommitLocation, CommitLocationToHashRequest, CommitRevlogDataRequest};
use crate::complete_tree::CompleteTreeRequest;
use crate::file::FileRequest;
use crate::history::HistoryRequest;
use crate::metadata::{DirectoryMetadataRequest, FileMetadataRequest};
use crate::tree::{TreeAttributes, TreeRequest};

/// Parse a `CommitRevlogDataRequest` from JSON.
///
/// Example request:
/// ```json
/// {
///   "hgids": [
///     "1bb6c3e46bcb872d5d469230350e8a7fae8f5764",
///     "72b2678d2c0674d295d1b8d758886caeecbdaff2"
///   ]
/// }
/// ```
pub fn parse_commit_revlog_data_req(json: &Value) -> Result<CommitRevlogDataRequest> {
    let json = json.as_object().context("input must be a JSON object")?;
    let hgids = parse_hashes(json.get("hgids").context("missing field hgids")?)?;
    Ok(CommitRevlogDataRequest { hgids })
}

/// Parse a `LocationToHashRequest` from JSON.
///
/// Example request:
/// ```json
/// {
///   "locations": [{
///     "known_descendant": "159a8912de890112b8d6005999cdf4988213fb2f",
///     "distance_to_descendant": 1,
///     "count": 2
///   }]
/// }
pub fn parse_commit_location_to_hash_req(json: &Value) -> Result<CommitLocationToHashRequest> {
    let json = json.as_object().context("input must be a JSON object")?;
    let locations_json = json
        .get("locations")
        .context("missing field locations")?
        .as_array()
        .context("field locations is not an array")?;
    let mut locations = Vec::new();
    for entry in locations_json {
        let known_descendent = HgId::from_str(
            entry
                .get("known_descendant")
                .context("missing field descendant")?
                .as_str()
                .context("field descendant is not a string")?,
        )
        .context("could not be parsed as HgId")?;
        let distance_to_descendant = entry
            .get("distance_to_descendant")
            .context("missing field distance_to_descendant")?
            .as_u64()
            .context("field distance_to_descendant is not a valid u64 number")?;
        let count = entry
            .get("count")
            .context("missing field count")?
            .as_u64()
            .context("field count is not a valid u64 number")?;
        let location = CommitLocation::new(known_descendent, distance_to_descendant, count);
        locations.push(location);
    }
    Ok(CommitLocationToHashRequest { locations })
}

/// Parse a `FileRequest` from JSON.
///
/// The request is represented as a JSON object containing a "keys" field
/// consisting of an array of path/filenode pairs.
///
/// Example request:
///
/// ```json
/// {
///   "keys": [
///     ["path/to/file_1", "48f43af456d770b6a78e1ace628319847e05cc24"],
///     ["path/to/file_2", "7dcd6ede35eaaa5b1b16a341b19993e59f9b0dbf"],
///     ["path/to/file_3", "218d708a9f8c3e37cfd7ab916c537449ac5419cd"],
///   ]
/// }
/// ```
///
pub fn parse_file_req(json: &Value) -> Result<FileRequest> {
    let json = json.as_object().context("input must be a JSON object")?;
    let keys = json.get("keys").context("missing field: keys")?;

    Ok(FileRequest {
        keys: parse_keys(keys)?,
    })
}

/// Parse a `TreeRequest` from JSON.
///
/// The request is represented as a JSON object containing a "keys" field
/// consisting of an array of path/filenode pairs.
///
/// Example request:
///
/// ```json
/// {
///   "keys": [
///     ["path/to/file_1", "48f43af456d770b6a78e1ace628319847e05cc24"],
///     ["path/to/file_2", "7dcd6ede35eaaa5b1b16a341b19993e59f9b0dbf"],
///     ["path/to/file_3", "218d708a9f8c3e37cfd7ab916c537449ac5419cd"],
///   ]
/// }
/// ```
///
pub fn parse_tree_req(json: &Value) -> Result<TreeRequest> {
    let json = json.as_object().context("input must be a JSON object")?;
    let keys = json.get("keys").context("missing field: keys")?;
    let attrs = optional_default_field(json, "attributes")?;

    Ok(TreeRequest {
        keys: parse_keys(keys)?,
        attributes: attrs,
    })
}

/// Parse a `TreeAttributes` from JSON.
///
/// The request is represented as a JSON object containing a "keys" field
/// consisting of an array of path/filenode pairs.
///
/// Example request:
///
/// ```json
/// {
///   "manifest_blob": true,
///   "parents": true,
///   "child_metadata": false,
/// }
/// ```
///
pub fn parse_tree_attrs(json: &Value) -> Result<TreeAttributes> {
    let json = json.as_object().context("input must be a JSON object")?;
    let manifest_blob = optional_bool_field(json, "manifest_blob")?;
    let parents = optional_bool_field(json, "parents")?;
    let child_metadata = optional_bool_field(json, "child_metadata")?;

    Ok(TreeAttributes {
        manifest_blob,
        parents,
        child_metadata,
    })
}

/// Parse a `HistoryRequest` from JSON.
///
/// The request is represented as a JSON object containing a required
/// "keys" field consisting of an array of path/filenode pairs (similar
/// to a data request) as well as an optional length parameter.
///
/// Example request:
///
/// ```json
/// {
///   "keys": [
///     ["path/to/file_1", "48f43af456d770b6a78e1ace628319847e05cc24"],
///     ["path/to/file_2", "7dcd6ede35eaaa5b1b16a341b19993e59f9b0dbf"],
///     ["path/to/file_3", "218d708a9f8c3e37cfd7ab916c537449ac5419cd"],
///   ],
///   "length": 1,
/// }
/// ```
pub fn parse_history_req(value: &Value) -> Result<HistoryRequest> {
    let value = value.as_object().context("input must be a JSON object")?;
    let length = value
        .get("length")
        .and_then(|d| d.as_u64())
        .map(|d| d as u32);
    let keys = {
        let json_keys = value.get("keys").context("missing field: keys")?;
        parse_keys(json_keys)?
    };

    Ok(HistoryRequest { keys, length })
}

/// Parse a `CompleteTreeRequest` from JSON.
///
/// The request is represented as a JSON object containing the fields
/// needed for a "gettreepack"-style complete tree request. Note that
/// it is generally preferred to request trees using a `DataRequest`
/// for the desired tree nodes, as this is a lot less expensive than
/// fetching complete trees.
///
/// Example request:
///
/// ```json
/// {
///     "rootdir": "path/to/root/dir",
///     "mfnodes": [
///         "8722607999fc5ce35e9af56e6da2c823923291dd",
///         "b7d7ffb1a37c86f00558ff132e57c56bca29dc04"
///     ],
///     "basemfnodes": [
///         "26d6acbabf823b844917f04cfbe6747c80983119",
///         "111caaed68164b939f6e2f58680b462ebc3174c7"
///     ],
///     "depth": 1
/// }
/// ```
///
pub fn parse_complete_tree_req(value: &Value) -> Result<CompleteTreeRequest> {
    let obj = value.as_object().context("input must be a JSON object")?;

    let rootdir = obj.get("rootdir").context("missing field: rootdir")?;
    let rootdir = rootdir.as_str().context("rootdir field must be a string")?;
    let rootdir = RepoPathBuf::from_string(rootdir.to_string())?;

    let mfnodes = obj.get("mfnodes").context("missing field: mfnodes")?;
    let mfnodes = parse_hashes(mfnodes)?;

    let basemfnodes = obj
        .get("basemfnodes")
        .context("missing field: basemfnodes")?;
    let basemfnodes = parse_hashes(basemfnodes)?;

    let depth = obj
        .get("depth")
        .and_then(|d| d.as_u64())
        .map(|d| d as usize);

    Ok(CompleteTreeRequest {
        rootdir,
        mfnodes,
        basemfnodes,
        depth,
    })
}

pub fn parse_file_metadata_req(json: &Value) -> Result<FileMetadataRequest> {
    let json = json.as_object().context("input must be a JSON object")?;

    let with_revisionstore_flags = optional_bool_field(json, "with_revisionstore_flags")?;
    let with_content_id = optional_bool_field(json, "with_content_id")?;
    let with_file_type = optional_bool_field(json, "with_file_type")?;
    let with_size = optional_bool_field(json, "with_size")?;
    let with_content_sha1 = optional_bool_field(json, "with_content_sha1")?;
    let with_content_sha256 = optional_bool_field(json, "with_content_sha256")?;

    Ok(FileMetadataRequest {
        with_revisionstore_flags,
        with_content_id,
        with_file_type,
        with_size,
        with_content_sha1,
        with_content_sha256,
    })
}

pub fn parse_directory_metadata_req(json: &Value) -> Result<DirectoryMetadataRequest> {
    let json = json.as_object().context("input must be a JSON object")?;

    let with_fsnode_id = optional_bool_field(json, "with_fsnode_id")?;
    let with_simple_format_sha1 = optional_bool_field(json, "with_simple_format_sha1")?;
    let with_simple_format_sha256 = optional_bool_field(json, "with_simple_format_sha256")?;
    let with_child_files_count = optional_bool_field(json, "with_child_files_count")?;
    let with_child_files_total_size = optional_bool_field(json, "with_child_files_total_size")?;
    let with_child_dirs_count = optional_bool_field(json, "with_child_dirs_count")?;
    let with_descendant_files_count = optional_bool_field(json, "with_descendant_files_count")?;
    let with_descendant_files_total_size =
        optional_bool_field(json, "with_descendant_files_total_size")?;

    Ok(DirectoryMetadataRequest {
        with_fsnode_id,
        with_simple_format_sha1,
        with_simple_format_sha256,
        with_child_files_count,
        with_child_files_total_size,
        with_child_dirs_count,
        with_descendant_files_count,
        with_descendant_files_total_size,
    })
}

fn parse_keys(value: &Value) -> Result<Vec<Key>> {
    let arr = value.as_array().context("input must be a JSON array")?;

    let mut keys = Vec::new();
    for i in arr.iter() {
        let json_key = i
            .as_array()
            .context("array items must be [path, hash] arrays")?;

        ensure!(
            json_key.len() == 2,
            "array items must be [path, hash] arrays"
        );

        // Cast slice into 2-element array reference so we can destructure it.
        let [path, hash] = <&[_; 2]>::try_from(&json_key[..2])?;

        let path = path.as_str().context("path must be a string")?;
        let hash = hash.as_str().context("hash must be a string")?;

        let key = make_key(&path, hash)?;
        keys.push(key);
    }

    Ok(keys)
}

fn parse_hashes(value: &Value) -> Result<Vec<HgId>> {
    let array = value
        .as_array()
        .context("node hashes must be a passed as an array")?;
    let mut hashes = Vec::new();
    for hex in array {
        let hex = hex.as_str().context("node hashes must be strings")?;
        let hash = HgId::from_str(hex)?;
        hashes.push(hash);
    }
    Ok(hashes)
}

fn make_key(path: &str, hash: &str) -> Result<Key> {
    let path = if path.is_empty() {
        RepoPathBuf::new()
    } else {
        RepoPathBuf::from_string(path.to_string())?
    };
    let hgid = HgId::from_str(hash)?;
    Ok(Key::new(path, hgid))
}

fn optional_bool_field(json: &Map<String, Value>, field: &str) -> Result<bool> {
    Ok(json
        .get(field)
        .map(|w| {
            w.as_bool()
                .context(format!("{} {}", field, "field must be a bool"))
        })
        .transpose()?
        .unwrap_or_default())
}

fn optional_default_field<T: Default + FromJson>(
    json: &Map<String, Value>,
    field: &str,
) -> Result<T> {
    Ok(json
        .get(field)
        .map(|w| T::from_json(w).context(format!("{} {}", field, "had incorrect type")))
        .transpose()?
        .unwrap_or_default())
}

pub trait FromJson: Sized {
    fn from_json(json: &Value) -> Result<Self>;
}

impl FromJson for FileRequest {
    fn from_json(json: &Value) -> Result<Self> {
        parse_file_req(json)
    }
}

impl FromJson for TreeRequest {
    fn from_json(json: &Value) -> Result<Self> {
        parse_tree_req(json)
    }
}

impl FromJson for TreeAttributes {
    fn from_json(json: &Value) -> Result<Self> {
        parse_tree_attrs(json)
    }
}

impl FromJson for HistoryRequest {
    fn from_json(json: &Value) -> Result<Self> {
        parse_history_req(json)
    }
}

impl FromJson for CompleteTreeRequest {
    fn from_json(json: &Value) -> Result<Self> {
        parse_complete_tree_req(json)
    }
}

impl FromJson for CommitLocationToHashRequest {
    fn from_json(json: &Value) -> Result<Self> {
        parse_commit_location_to_hash_req(json)
    }
}

impl FromJson for CommitRevlogDataRequest {
    fn from_json(json: &Value) -> Result<Self> {
        parse_commit_revlog_data_req(json)
    }
}

pub trait ToJson {
    fn to_json(&self) -> Value;
}

impl ToJson for HgId {
    fn to_json(&self) -> Value {
        json!(self.to_hex())
    }
}

impl ToJson for Key {
    fn to_json(&self) -> Value {
        json!([&self.path, self.hgid.to_json()])
    }
}

impl<T: ToJson> ToJson for Vec<T> {
    fn to_json(&self) -> Value {
        self.iter().map(ToJson::to_json).collect::<Vec<_>>().into()
    }
}

impl ToJson for TreeRequest {
    fn to_json(&self) -> Value {
        json!({
            "keys": self.keys.to_json(),
            "attributes": self.attributes.to_json(),
        })
    }
}

impl ToJson for TreeAttributes {
    fn to_json(&self) -> Value {
        json!({
            "manifest_blob": self.manifest_blob,
            "parents": self.parents,
            "child_metadata": self.child_metadata,
        })
    }
}

impl ToJson for FileRequest {
    fn to_json(&self) -> Value {
        json!({ "keys": self.keys.to_json() })
    }
}

impl ToJson for HistoryRequest {
    fn to_json(&self) -> Value {
        json!({ "keys": self.keys.to_json(), "length": self.length })
    }
}

impl ToJson for CompleteTreeRequest {
    fn to_json(&self) -> Value {
        json!({
            "rootdir": self.rootdir,
            "mfnodes": self.mfnodes.to_json(),
            "basemfnodes": self.basemfnodes.to_json(),
            "depth": self.depth,
        })
    }
}

impl ToJson for FileMetadataRequest {
    fn to_json(&self) -> Value {
        json!({
            "with_revisionstore_flags": self.with_revisionstore_flags,
            "with_content_id": self.with_content_id,
            "with_file_type": self.with_file_type,
            "with_size": self.with_size,
            "with_content_sha1": self.with_content_sha1,
            "with_content_sha256": self.with_content_sha256,
        })
    }
}

impl ToJson for DirectoryMetadataRequest {
    fn to_json(&self) -> Value {
        json!({
            "with_fsnode_id": self.with_fsnode_id,
            "with_simple_format_sha1": self.with_simple_format_sha1,
            "with_simple_format_sha256": self.with_simple_format_sha256,
            "with_child_files_count": self.with_child_files_count,
            "with_child_files_total_size": self.with_child_files_total_size,
            "with_child_dirs_count": self.with_child_dirs_count,
            "with_descendant_files_count": self.with_descendant_files_count,
            "with_descendant_files_total_size": self.with_descendant_files_total_size,
        })
    }
}

impl ToJson for CommitLocation {
    fn to_json(&self) -> Value {
        json!({
            "known_descendant": self.known_descendant.to_json(),
            "distance_to_descendant": self.distance_to_descendant,
            "count": self.count,
        })
    }
}

impl ToJson for CommitLocationToHashRequest {
    fn to_json(&self) -> Value {
        json!({
            "locations": self.locations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn test_file_req_roundtrip(req: FileRequest) -> bool {
        let json = req.to_json();
        req == FileRequest::from_json(&json).unwrap()
    }

    #[quickcheck]
    fn test_tree_req_roundtrip(req: TreeRequest) -> bool {
        let json = req.to_json();
        req == TreeRequest::from_json(&json).unwrap()
    }

    #[quickcheck]
    fn test_history_req_roundtrip(req: HistoryRequest) -> bool {
        let json = req.to_json();
        req == HistoryRequest::from_json(&json).unwrap()
    }

    #[quickcheck]
    fn test_complete_tree_req_roundtrip(req: CompleteTreeRequest) -> bool {
        let json = req.to_json();
        req == CompleteTreeRequest::from_json(&json).unwrap()
    }
}
