// Copyright 2019 Facebook, Inc.

use std::path::{Path, PathBuf};

use failure::{Error, Fallible};
use futures::future::poll_fn;
use tokio::prelude::*;
use tokio_threadpool::blocking;

use revisionstore::{unionhistorystore::UnionHistoryStore, HistoryPack, HistoryStore};

use crate::asynchistorystore::AsyncHistoryStore;

pub type AsyncUnionHistoryStore<T> = AsyncHistoryStore<UnionHistoryStore<T>>;

fn new_store<T: HistoryStore + Send + 'static>(
    packs: Vec<PathBuf>,
    builder: impl Fn(&Path) -> Fallible<T> + Send + 'static,
) -> impl Future<Item = AsyncUnionHistoryStore<T>, Error = Error> + Send + 'static {
    poll_fn({
        move || {
            blocking(|| {
                let mut store = UnionHistoryStore::new();

                for pack in packs.iter() {
                    store.add(builder(&pack)?);
                }

                Ok(store)
            })
        }
    })
    .from_err()
    .and_then(|res| res)
    .map(move |unionstore| AsyncUnionHistoryStore::new_(unionstore))
}

impl AsyncUnionHistoryStore<HistoryPack> {
    pub fn new(
        packs: Vec<PathBuf>,
    ) -> impl Future<Item = AsyncUnionHistoryStore<HistoryPack>, Error = Error> + Send + 'static
    {
        new_store(packs, HistoryPack::new)
    }
}
