// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::collections::HashMap;
use std::io::Write;
use std::str;
use std::str::FromStr;

use bytes::Bytes;
use itertools::Itertools;

use mercurial_types::{HgBlob, HgBlobNode, HgNodeHash, MPath};
use mononoke_types::{FileContents, hash::Sha256};

use errors::*;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct File {
    node: HgBlobNode,
}

const META_MARKER: &[u8] = b"\x01\n";
const COPY_PATH_KEY: &[u8] = b"copy";
const COPY_REV_KEY: &[u8] = b"copyrev";
const META_SZ: usize = 2;

impl File {
    pub fn new<B: Into<HgBlob>>(blob: B, p1: Option<&HgNodeHash>, p2: Option<&HgNodeHash>) -> Self {
        let node = HgBlobNode::new(blob, p1, p2);
        File { node }
    }

    // (there's a use case for not providing parents, so should parents not be inside the file?)
    #[inline]
    pub fn data_only<B: Into<HgBlob>>(blob: B) -> Self {
        Self::new(blob, None, None)
    }

    // HgBlobNode should probably go away eventually, probably? So mark this private.
    #[inline]
    pub(crate) fn from_blobnode(node: HgBlobNode) -> Self {
        File { node }
    }

    // Note that this function drops empty metadata. For lossless preservation, use the metadata
    // function instead.
    fn extract_meta(file: &[u8]) -> (&[u8], usize) {
        if file.len() < META_SZ {
            return (&[], 0);
        }
        if &file[..META_SZ] != META_MARKER {
            (&[], 0)
        } else {
            let metasz = &file[META_SZ..]
                .iter()
                .enumerate()
                .tuple_windows()
                .find(|&((_, a), (_, b))| *a == META_MARKER[0] && *b == META_MARKER[1])
                .map(|((idx, _), _)| idx + META_SZ * 2)
                .unwrap_or(META_SZ); // XXX malformed if None - unterminated metadata

            let metasz = *metasz;
            if metasz >= META_SZ * 2 {
                (&file[META_SZ..metasz - META_SZ], metasz)
            } else {
                (&[], metasz)
            }
        }
    }

    fn parse_to_hash_map<'a>(content: &'a [u8], delimiter: &[u8]) -> HashMap<&'a [u8], &'a [u8]> {
        let mut kv = HashMap::new();
        let delimiter_len = delimiter.len();

        for line in content.split(|c| *c == b'\n') {
            if line.len() < delimiter_len {
                continue;
            }

            // split on "delimiter" - no quoting within key/value
            for idx in 0..line.len() - delimiter_len + 1 {
                if &line[idx..idx + delimiter_len] == delimiter {
                    kv.insert(&line[..idx], &line[idx + delimiter_len..]);
                    break;
                }
            }
        }
        kv
    }

    fn parse_meta(file: &[u8]) -> HashMap<&[u8], &[u8]> {
        let (meta, _) = Self::extract_meta(file);

        // Yay, Mercurial has yet another ad-hoc encoding. This one is kv pairs separated by \n,
        // with ": " separating the key and value
        Self::parse_to_hash_map(meta, &[b':', b' '])
    }

    fn parse_content_to_lfs_hash_map(content: &[u8]) -> HashMap<&[u8], &[u8]> {
        Self::parse_to_hash_map(content, &[b' '])
    }

    pub fn copied_from(&self) -> Result<Option<(MPath, HgNodeHash)>> {
        if !self.node.maybe_copied() {
            return Ok(None);
        }

        let buf = self.node.as_blob().as_slice();

        Self::get_copied_from(&Self::parse_meta(buf))
    }

    fn get_copied_from_with_keys(
        meta: &HashMap<&[u8], &[u8]>,
        copy_path_key: &'static [u8],
        copy_rev_key: &'static [u8],
    ) -> Result<Option<(MPath, HgNodeHash)>> {
        let path = meta.get(copy_path_key).cloned().map(MPath::new);
        let nodeid = meta.get(copy_rev_key)
            .and_then(|rev| str::from_utf8(rev).ok())
            .and_then(|rev| rev.parse().ok());
        match (path, nodeid) {
            (Some(Ok(path)), Some(nodeid)) => Ok(Some((path, nodeid))),
            (Some(Err(e)), _) => Err(e.context("invalid path in copy metadata").into()),
            _ => Ok(None),
        }
    }

    pub(crate) fn get_copied_from(
        meta: &HashMap<&[u8], &[u8]>,
    ) -> Result<Option<(MPath, HgNodeHash)>> {
        Self::get_copied_from_with_keys(meta, COPY_PATH_KEY, COPY_REV_KEY)
    }

    pub fn generate_metadata<T>(
        copy_from: Option<&(MPath, HgNodeHash)>,
        file_contents: &FileContents,
        buf: &mut T,
    ) -> Result<()>
    where
        T: Write,
    {
        match copy_from {
            None => if file_contents.starts_with(META_MARKER) {
                // If the file contents starts with META_MARKER, the metadata must be
                // written out to avoid ambiguity.
                buf.write_all(META_MARKER)?;
                buf.write_all(META_MARKER)?;
            },
            Some((path, version)) => {
                buf.write_all(META_MARKER)?;
                buf.write_all(COPY_PATH_KEY)?;
                buf.write_all(b": ")?;
                path.generate(buf)?;
                buf.write_all(b"\n")?;

                buf.write_all(COPY_REV_KEY)?;
                buf.write_all(b": ")?;
                buf.write_all(version.to_hex().as_ref())?;
                buf.write_all(b"\n")?;
                buf.write_all(META_MARKER)?;
            }
        };
        Ok(())
    }

    pub fn content(&self) -> &[u8] {
        let data = self.node.as_blob().as_slice();
        let (_, off) = Self::extract_meta(data);
        &data[off..]
    }

    pub fn metadata(&self) -> Bytes {
        let data = self.node.as_blob().as_inner();
        let (_, off) = Self::extract_meta(data);
        data.slice_to(off)
    }

    pub fn file_contents(&self) -> FileContents {
        let data = self.node.as_blob().as_inner();
        let (_, off) = Self::extract_meta(data);
        FileContents::Bytes(data.slice_from(off))
    }

    pub fn size(&self) -> usize {
        // XXX This doesn't really help because the HgBlobNode will have already been constructed
        // with the content so a size-only query will have already done too much work.
        if self.node.maybe_copied() {
            self.content().len()
        } else {
            self.node.size()
        }
    }

    pub fn get_lfs_content(&self) -> Result<LFSContent> {
        let data = self.node.as_blob().as_inner();
        let (_, off) = Self::extract_meta(data);

        Self::get_lfs_struct(&Self::parse_content_to_lfs_hash_map(&data.slice_from(off)))
    }

    fn parse_mandatory_lfs(contents: &HashMap<&[u8], &[u8]>) -> Result<(String, Sha256, u64)> {
        let version = contents
            .get(VERSION)
            .and_then(|s| str::from_utf8(*s).ok())
            .map(|s| s.to_string())
            .ok_or(ErrorKind::IncorrectLfsFileContent(
                "VERSION mandatory field parsing failed in Lfs file content".to_string(),
            ))?;

        let oid = contents
            .get(OID)
            .and_then(|s| str::from_utf8(*s).ok())
            .and_then(|s| {
                let prefix_len = SHA256_PREFIX.len();

                let check = prefix_len <= s.len() && &s[..prefix_len].as_bytes() == &SHA256_PREFIX;
                if check {
                    Some(s[prefix_len..].to_string())
                } else {
                    None
                }
            })
            .and_then(|s| Sha256::from_str(&s).ok())
            .ok_or(ErrorKind::IncorrectLfsFileContent(
                "OID mandatory field parsing failed in Lfs file content".to_string(),
            ))?;
        let size = contents
            .get(SIZE)
            .and_then(|s| str::from_utf8(*s).ok())
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or(ErrorKind::IncorrectLfsFileContent(
                "SIZE mandatory field parsing failed in Lfs file content".to_string(),
            ))?;
        Ok((version, oid, size))
    }

    fn get_lfs_struct(contents: &HashMap<&[u8], &[u8]>) -> Result<LFSContent> {
        Self::parse_mandatory_lfs(contents)
            .and_then(|(version, oid, size)| {
                Self::get_copied_lfs(contents).map(move |copy_from| (version, oid, size, copy_from))
            })
            .map(|(version, oid, size, copy_from)| LFSContent {
                _version: version,
                oid,
                size,
                copy_from,
            })
    }

    fn get_copied_lfs(contents: &HashMap<&[u8], &[u8]>) -> Result<Option<(MPath, HgNodeHash)>> {
        Self::get_copied_from_with_keys(contents, HGCOPY, HGCOPYREV)
    }
}

const VERSION: &[u8] = b"version";
const OID: &[u8] = b"oid";
const SIZE: &[u8] = b"size";
const HGCOPY: &[u8] = b"x-hg-copy";
const HGCOPYREV: &[u8] = b"x-hg-copyrev";
const _ISBINARY: &[u8] = b"x-is-binary";
const SHA256_PREFIX: &[u8] = b"sha256:";

// See [https://www.mercurial-scm.org/wiki/LfsPlan], By default, version, oid and size are required
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LFSContent {
    // mandatory fields
    _version: String,
    oid: Sha256,
    size: u64,

    // copy fields
    copy_from: Option<(MPath, HgNodeHash)>,
}

impl LFSContent {
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn oid(&self) -> Sha256 {
        self.oid.clone()
    }

    pub fn copy_from(&self) -> Option<(MPath, HgNodeHash)> {
        self.copy_from.clone()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use mercurial_types_mocks::nodehash::*;

    #[test]
    fn extract_meta_sz() {
        assert_eq!(META_SZ, META_MARKER.len())
    }

    #[test]
    fn extract_meta_0() {
        const DATA: &[u8] = b"foo - no meta";

        assert_eq!(File::extract_meta(DATA), (&[][..], 0));
    }

    #[test]
    fn extract_meta_1() {
        const DATA: &[u8] = b"\x01\n\x01\nfoo - empty meta";

        assert_eq!(File::extract_meta(DATA), (&[][..], 4));
    }

    #[test]
    fn extract_meta_2() {
        const DATA: &[u8] = b"\x01\nabc\x01\nfoo - some meta";

        assert_eq!(File::extract_meta(DATA), (&b"abc"[..], 7));
    }

    #[test]
    fn extract_meta_3() {
        const DATA: &[u8] = b"\x01\nfoo - bad unterminated meta";

        assert_eq!(File::extract_meta(DATA), (&[][..], 2));
    }

    #[test]
    fn extract_meta_4() {
        const DATA: &[u8] = b"\x01\n\x01\n\x01\nfoo - bad unterminated meta";

        assert_eq!(File::extract_meta(DATA), (&[][..], 4));
    }

    #[test]
    fn extract_meta_5() {
        const DATA: &[u8] = b"\x01\n\x01\n";

        assert_eq!(File::extract_meta(DATA), (&[][..], 4));
    }

    #[test]
    fn parse_meta_0() {
        const DATA: &[u8] = b"foo - no meta";

        assert!(File::parse_meta(DATA).is_empty())
    }

    #[test]
    fn test_meta_1() {
        const DATA: &[u8] = b"\x01\n\x01\nfoo - empty meta";

        assert!(File::parse_meta(DATA).is_empty())
    }

    #[test]
    fn test_meta_2() {
        const DATA: &[u8] = b"\x01\nfoo: bar\x01\nfoo - empty meta";

        let kv: Vec<_> = File::parse_meta(DATA).into_iter().collect();

        assert_eq!(kv, vec![(b"foo".as_ref(), b"bar".as_ref())])
    }

    #[test]
    fn test_meta_3() {
        const DATA: &[u8] = b"\x01\nfoo: bar\nblim: blop: blap\x01\nfoo - empty meta";

        let mut kv: Vec<_> = File::parse_meta(DATA).into_iter().collect();
        kv.as_mut_slice().sort();

        assert_eq!(
            kv,
            vec![
                (b"blim".as_ref(), b"blop: blap".as_ref()),
                (b"foo".as_ref(), b"bar".as_ref()),
            ]
        )
    }

    #[test]
    fn test_hash_meta_delimiter_only_0() {
        const DELIMITER: &[u8] = b"DELIMITER";
        const DATA: &[u8] = b"DELIMITER\n";

        let mut kv: Vec<_> = File::parse_to_hash_map(DATA, DELIMITER)
            .into_iter()
            .collect();
        kv.as_mut_slice().sort();
        assert_eq!(kv, vec![(b"".as_ref(), b"".as_ref())])
    }

    #[test]
    fn test_hash_meta_delimiter_only_1() {
        const DELIMITER: &[u8] = b"DELIMITER";
        const DATA: &[u8] = b"DELIMITER";

        let mut kv: Vec<_> = File::parse_to_hash_map(DATA, DELIMITER)
            .into_iter()
            .collect();
        kv.as_mut_slice().sort();
        assert_eq!(kv, vec![(b"".as_ref(), b"".as_ref())])
    }

    #[test]
    fn test_hash_meta_delimiter_short_0() {
        const DELIMITER: &[u8] = b"DELIMITER";
        const DATA: &[u8] = b"DELIM";

        let mut kv: Vec<_> = File::parse_to_hash_map(DATA, DELIMITER)
            .into_iter()
            .collect();
        assert!(kv.as_mut_slice().is_empty())
    }

    #[test]
    fn test_hash_meta_delimiter_short_1() {
        const DELIMITER: &[u8] = b"DELIMITER";
        const DATA: &[u8] = b"\n";

        let mut kv: Vec<_> = File::parse_to_hash_map(DATA, DELIMITER)
            .into_iter()
            .collect();
        assert!(kv.as_mut_slice().is_empty())
    }

    #[test]
    fn generate_metadata_0() {
        const FILE_CONTENTS: &[u8] = b"foobar";
        let file_contents = FileContents::Bytes(Bytes::from(FILE_CONTENTS));
        let mut out: Vec<u8> = vec![];
        File::generate_metadata(None, &file_contents, &mut out)
            .expect("Vec::write_all should succeed");
        assert_eq!(out.as_slice(), &b""[..]);

        let mut out: Vec<u8> = vec![];
        File::generate_metadata(
            Some(&(MPath::new("foo").unwrap(), ONES_HASH)),
            &file_contents,
            &mut out,
        ).expect("Vec::write_all should succeed");
        assert_eq!(
            out.as_slice(),
            &b"\x01\ncopy: foo\ncopyrev: 1111111111111111111111111111111111111111\n\x01\n"[..]
        );
    }

    #[test]
    fn generate_metadata_1() {
        // The meta marker in the beginning should cause metadata to unconditionally be emitted.
        const FILE_CONTENTS: &[u8] = b"\x01\nfoobar";
        let file_contents = FileContents::Bytes(Bytes::from(FILE_CONTENTS));
        let mut out: Vec<u8> = vec![];
        File::generate_metadata(None, &file_contents, &mut out)
            .expect("Vec::write_all should succeed");
        assert_eq!(out.as_slice(), &b"\x01\n\x01\n"[..]);

        let mut out: Vec<u8> = vec![];
        File::generate_metadata(
            Some(&(MPath::new("foo").unwrap(), ONES_HASH)),
            &file_contents,
            &mut out,
        ).expect("Vec::write_all should succeed");
        assert_eq!(
            out.as_slice(),
            &b"\x01\ncopy: foo\ncopyrev: 1111111111111111111111111111111111111111\n\x01\n"[..]
        );
    }

    #[test]
    fn test_get_lfs_hash_map() {
        const DATA: &[u8] = b"version https://git-lfs.github.com/spec/v1\noid sha256:27c0a92fc51290e3227bea4dd9e780c5035f017de8d5ddfa35b269ed82226d97\nsize 17";

        let mut kv: Vec<_> = File::parse_content_to_lfs_hash_map(DATA)
            .into_iter()
            .collect();
        kv.as_mut_slice().sort();

        assert_eq!(
            kv,
            vec![
                (
                    b"oid".as_ref(),
                    b"sha256:27c0a92fc51290e3227bea4dd9e780c5035f017de8d5ddfa35b269ed82226d97"
                        .as_ref(),
                ),
                (b"size".as_ref(), b"17".as_ref()),
                (
                    b"version".as_ref(),
                    b"https://git-lfs.github.com/spec/v1".as_ref(),
                ),
            ]
        )
    }

    #[test]
    fn test_get_lfs_struct_0() {
        let mut kv = HashMap::new();
        kv.insert(
            b"version".as_ref(),
            b"https://git-lfs.github.com/spec/v1".as_ref(),
        );
        kv.insert(
            b"oid".as_ref(),
            b"sha256:27c0a92fc51290e3227bea4dd9e780c5035f017de8d5ddfa35b269ed82226d97".as_ref(),
        );
        kv.insert(b"size".as_ref(), b"17".as_ref());
        let lfs = File::get_lfs_struct(&kv);

        assert_eq!(
            lfs.unwrap(),
            LFSContent {
                _version: "https://git-lfs.github.com/spec/v1".to_string(),
                oid: Sha256::from_str(
                    "27c0a92fc51290e3227bea4dd9e780c5035f017de8d5ddfa35b269ed82226d97"
                ).unwrap(),
                size: 17,
                copy_from: None,
            }
        )
    }

    #[test]
    fn test_get_lfs_struct_wrong_small_sha256() {
        let mut kv = HashMap::new();
        kv.insert(
            b"version".as_ref(),
            b"https://git-lfs.github.com/spec/v1".as_ref(),
        );
        kv.insert(b"oid".as_ref(), b"sha256:123".as_ref());
        kv.insert(b"size".as_ref(), b"17".as_ref());
        let lfs = File::get_lfs_struct(&kv);

        assert_eq!(lfs.is_err(), true)
    }

    #[test]
    fn test_get_lfs_struct_wrong_size() {
        let mut kv = HashMap::new();
        kv.insert(
            b"version".as_ref(),
            b"https://git-lfs.github.com/spec/v1".as_ref(),
        );
        kv.insert(
            b"oid".as_ref(),
            b"sha256:27c0a92fc51290e3227bea4dd9e780c5035f017de8d5ddfa35b269ed82226d97".as_ref(),
        );
        kv.insert(b"size".as_ref(), b"wrong_size_length".as_ref());
        let lfs = File::get_lfs_struct(&kv);

        assert_eq!(lfs.is_err(), true)
    }

    #[test]
    fn test_get_lfs_struct_non_all_mandatory_fields() {
        let mut kv = HashMap::new();
        kv.insert(
            b"oid".as_ref(),
            b"sha256:27c0a92fc51290e3227bea4dd9e780c5035f017de8d5ddfa35b269ed82226d97".as_ref(),
        );
        let lfs = File::get_lfs_struct(&kv);

        assert_eq!(lfs.is_err(), true)
    }

    quickcheck! {
        fn copy_info_roundtrip(
            copy_info: Option<(MPath, HgNodeHash)>,
            contents: FileContents
        ) -> bool {
            let mut buf = Vec::new();
            let result = File::generate_metadata(copy_info.as_ref(), &contents, &mut buf)
                .and_then(|_| {
                    File::get_copied_from(&File::parse_meta(&buf))
                });
            match result {
                Ok(out_copy_info) => copy_info == out_copy_info,
                _ => {
                    false
                }
            }
        }
    }
}
