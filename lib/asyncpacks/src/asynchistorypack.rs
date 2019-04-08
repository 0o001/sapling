// Copyright 2019 Facebook, Inc.

use std::path::PathBuf;

use failure::Error;
use futures::future::poll_fn;
use tokio::prelude::*;
use tokio_threadpool::blocking;

use revisionstore::HistoryPack;

use crate::asynchistorystore::AsyncHistoryStore;

pub type AsyncHistoryPack = AsyncHistoryStore<HistoryPack>;

impl AsyncHistoryPack {
    pub fn new(
        path: PathBuf,
    ) -> impl Future<Item = AsyncHistoryPack, Error = Error> + Send + 'static {
        poll_fn({ move || blocking(|| HistoryPack::new(&path)) })
            .from_err()
            .and_then(|res| res)
            .map(move |historypack| AsyncHistoryStore::new_(historypack))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use tempfile::TempDir;
    use tokio::runtime::Runtime;

    use cloned::cloned;
    use futures_ext::FutureExt;
    use revisionstore::{Ancestors, HistoryPackVersion, MutableHistoryPack, MutablePack};
    use types::{testutil::*, Key, NodeInfo};

    fn make_historypack(
        tempdir: &TempDir,
        nodes: &HashMap<Key, NodeInfo>,
    ) -> impl Future<Item = AsyncHistoryPack, Error = Error> + 'static {
        let mut mutpack = MutableHistoryPack::new(tempdir.path(), HistoryPackVersion::One).unwrap();
        for (ref key, ref info) in nodes.iter() {
            mutpack.add(key.clone(), info.clone()).unwrap();
        }

        let path = mutpack.close().unwrap();
        AsyncHistoryPack::new(path)
    }

    // XXX: we should unify this and historypack.rs

    fn get_nodes() -> (HashMap<Key, NodeInfo>, HashMap<Key, Ancestors>) {
        let mut nodes = HashMap::new();
        let mut ancestor_map = HashMap::new();

        let file1 = "a";
        let file2 = "a/b";

        // Insert key 1
        let key1 = key(&file1, "2");;
        let info = NodeInfo {
            parents: [key(&file1, "1"), null_key(&file1)],
            linknode: node("101"),
        };
        nodes.insert(key1.clone(), info.clone());
        let mut ancestors = HashMap::new();
        ancestors.insert(key1.clone(), info.clone());
        ancestor_map.insert(key1.clone(), ancestors);

        // Insert key 2
        let key2 = key(&file2, "3");
        let info = NodeInfo {
            parents: [key(&file2, "5"), key(&file2, "6")],
            linknode: node("102"),
        };
        nodes.insert(key2.clone(), info.clone());
        let mut ancestors = HashMap::new();
        ancestors.insert(key2.clone(), info.clone());
        ancestor_map.insert(key2.clone(), ancestors);

        // Insert key 3
        let key3 = key(&file1, "4");
        let info = NodeInfo {
            parents: [key2.clone(), key1.clone()],
            linknode: node("102"),
        };
        nodes.insert(key3.clone(), info.clone());
        let mut ancestors = HashMap::new();
        ancestors.insert(key3.clone(), info.clone());
        ancestors.extend(ancestor_map.get(&key2).unwrap().clone());
        ancestors.extend(ancestor_map.get(&key1).unwrap().clone());
        ancestor_map.insert(key3.clone(), ancestors);

        (nodes, ancestor_map)
    }

    #[test]
    fn test_get_ancestors() {
        let tempdir = TempDir::new().unwrap();

        let (nodes, ancestors) = get_nodes();

        let mut work = make_historypack(&tempdir, &nodes).boxify();
        for (key, _) in nodes.iter() {
            cloned!(key, ancestors);
            work = work
                .and_then(move |historypack| {
                    historypack.get_ancestors(&key).map(move |response| {
                        assert_eq!(&response, ancestors.get(&key).unwrap());
                        historypack
                    })
                })
                .boxify();
        }

        let mut runtime = Runtime::new().unwrap();
        runtime.block_on(work).expect("get_ancestors failed");
    }

    #[test]
    fn test_get_node_info() {
        let tempdir = TempDir::new().unwrap();

        let (nodes, _) = get_nodes();

        let mut work = make_historypack(&tempdir, &nodes).boxify();
        for (key, info) in nodes.iter() {
            cloned!(key, info);
            work = work
                .and_then(move |historypack| {
                    historypack.get_node_info(&key).map(move |response| {
                        assert_eq!(response, info);
                        historypack
                    })
                })
                .boxify();
        }

        let mut runtime = Runtime::new().unwrap();
        runtime.block_on(work).expect("get_node_info failed");
    }
}
