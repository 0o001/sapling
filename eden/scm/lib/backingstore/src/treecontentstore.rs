/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{format_err, Result};
use bytes::Bytes;
use manifest_tree::TreeStore;
use revisionstore::{ContentStore, HgIdDataStore};
use types::{HgId, Key, RepoPath};

pub(crate) struct TreeContentStore {
    inner: ContentStore,
}

impl TreeContentStore {
    pub fn new(inner: ContentStore) -> Self {
        TreeContentStore { inner }
    }

    pub fn as_content_store(&self) -> &ContentStore {
        &self.inner
    }
}

impl TreeStore for TreeContentStore {
    fn get(&self, path: &RepoPath, hgid: HgId) -> Result<Bytes> {
        let key = Key::new(path.to_owned(), hgid);

        self.inner.get(&key).and_then(|opt| {
            opt.ok_or_else(|| format_err!("hgid: {:?} path: {:?} is not found.", hgid, path))
                .map(Into::into)
        })
    }

    fn insert(&self, _path: &RepoPath, _hgid: HgId, _data: Bytes) -> Result<()> {
        Err(format_err!("insert is not implemented."))
    }
}
