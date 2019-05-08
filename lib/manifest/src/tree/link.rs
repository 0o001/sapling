// Copyright 2019 Facebook, Inc.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::{collections::BTreeMap, sync::Arc};

use failure::{bail, format_err, Fallible};
use once_cell::sync::OnceCell;

use types::{Node, PathComponentBuf, RepoPath};

use crate::tree::{store, store::Store};
use crate::FileMetadata;

/// `Link` describes the type of nodes that tree manifest operates on.
#[derive(Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub enum Link {
    /// `Leaf` nodes store FileMetadata. They are terminal nodes and don't have any other
    /// information.
    Leaf(FileMetadata),
    /// `Ephemeral` nodes are inner nodes that have not been committed to storage. They are only
    /// available in memory. They need to be persisted to be available in future. They are the
    /// mutable type of an inner node. They store the contents of a directory that has been
    /// modified.
    Ephemeral(BTreeMap<PathComponentBuf, Link>),
    /// `Durable` nodes are inner nodes that come from storage. Their contents can be
    /// shared between multiple instances of Tree. They are lazily evaluated. Their children
    /// list will be read from storage only when it is accessed.
    Durable(Arc<DurableEntry>),
}
pub use self::Link::*;

// TODO: Use Vec instead of BTreeMap
/// The inner structure of a durable link. Of note is that failures are cached "forever".
// The interesting question about this structure is what do we do when we have a failure when
// reading from storage?
// We can cache the failure or we don't cache it. Caching it is mostly fine if we had an error
// reading from local storage or when deserializing. It is not the best option if our storage
// is remote and we hit a network blip. On the other hand we would not want to always retry when
// there is a failure on remote storage, we'd want to have a least an exponential backoff on
// retries. Long story short is that caching the failure is a reasonable place to start from.
#[derive(Debug)]
pub struct DurableEntry {
    pub node: Node,
    pub links: OnceCell<Fallible<BTreeMap<PathComponentBuf, Link>>>,
}

impl Link {
    pub fn durable(node: Node) -> Link {
        Link::Durable(Arc::new(DurableEntry::new(node)))
    }

    pub fn mut_ephemeral_links(
        &mut self,
        store: &dyn Store,
        parent: &RepoPath,
    ) -> Fallible<&mut BTreeMap<PathComponentBuf, Link>> {
        loop {
            match self {
                Leaf(_) => bail!("Encountered file where a directory was expected."),
                Ephemeral(ref mut links) => return Ok(links),
                Durable(ref entry) => {
                    let durable_links = entry.get_links(store, parent)?;
                    *self = Ephemeral(durable_links.clone());
                }
            }
        }
    }
}

impl DurableEntry {
    pub fn new(node: Node) -> Self {
        DurableEntry {
            node,
            links: OnceCell::new(),
        }
    }

    pub fn get_links(
        &self,
        store: &dyn Store,
        path: &RepoPath,
    ) -> Fallible<&BTreeMap<PathComponentBuf, Link>> {
        // TODO: be smarter around how failures are handled when reading from the store
        // Currently this loses the stacktrace
        let result = self.links.get_or_init(|| {
            let entry = store.get(path, &self.node)?;
            let mut links = BTreeMap::new();
            for element_result in entry.elements() {
                let element = element_result?;
                let link = match element.flag {
                    store::Flag::File(file_type) => {
                        Leaf(FileMetadata::new(element.node, file_type))
                    }
                    store::Flag::Directory => Link::durable(element.node),
                };
                links.insert(element.component, link);
            }
            Ok(links)
        });
        match result {
            Ok(links) => Ok(links),
            Err(error) => Err(format_err!("{}", error)),
        }
    }
}

// `PartialEq` can't be derived because `fallible::Error` does not implement `PartialEq`.
// It should also be noted that `self.links.get() != self.links.get()` can evaluate to true when
// `self.links` are being instantiated.
#[cfg(test)]
impl PartialEq for DurableEntry {
    fn eq(&self, other: &DurableEntry) -> bool {
        if self.node != other.node {
            return false;
        }
        match (self.links.get(), other.links.get()) {
            (None, None) => true,
            (Some(Ok(a)), Some(Ok(b))) => a == b,
            _ => false,
        }
    }
}
