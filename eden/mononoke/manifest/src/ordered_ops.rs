/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::marker::Unpin;

use anyhow::Error;
use borrowed::borrowed;
use bounded_traversal::OrderedTraversal;
use context::CoreContext;
use futures::future::FutureExt;
use futures::pin_mut;
use futures::stream::{BoxStream, StreamExt};
use mononoke_types::MPath;
use nonzero_ext::nonzero;

use crate::select::select_path_tree;
use crate::{Entry, Manifest, OrderedManifest, PathOrPrefix, PathTree, StoreLoadable};

pub trait ManifestOrderedOps<Store>
where
    Store: Sync + Send + Clone + 'static,
    Self: StoreLoadable<Store> + Clone + Send + Sync + Eq + Unpin + 'static,
    <Self as StoreLoadable<Store>>::Value: Manifest<TreeId = Self> + OrderedManifest + Send,
    <<Self as StoreLoadable<Store>>::Value as Manifest>::LeafId: Clone + Send + Eq + Unpin,
{
    fn find_entries_ordered<I, P>(
        &self,
        ctx: CoreContext,
        store: Store,
        paths_or_prefixes: I,
    ) -> BoxStream<
        'static,
        Result<
            (
                Option<MPath>,
                Entry<Self, <<Self as StoreLoadable<Store>>::Value as Manifest>::LeafId>,
            ),
            Error,
        >,
    >
    where
        I: IntoIterator<Item = P>,
        PathOrPrefix: From<P>,
    {
        let selector = select_path_tree(paths_or_prefixes);

        // Schedule a maximum of 256 concurrently unfolding directories.
        let schedule_max = nonzero!(256usize);

        // Allow queueing of up to 2,560 items, which would be 10 items per
        // directory at the maximum concurrency level.  Experiments show this
        // is a good balance of queueing items while not spending too long
        // determining what can be scheduled.
        let queue_max = nonzero!(2560usize);

        let init = Some((queue_max.get(), (self.clone(), selector, None, false)));
        (async_stream::stream! {
            let store = &store;
            borrowed!(ctx, store);
            let s = bounded_traversal::bounded_traversal_ordered_stream(
                schedule_max,
                queue_max,
                init,
                move |(manifest_id, selector, path, recursive)| {
                    let PathTree {
                        subentries,
                        value: select,
                    } = selector;

                    async move {
                        let manifest = manifest_id.load(ctx, &store).await?;

                        let mut output = Vec::new();

                        if recursive || select.is_recursive() {
                            output.push(OrderedTraversal::Output((
                                path.clone(),
                                Entry::Tree(manifest_id),
                            )));
                            for (name, entry) in manifest.list_weighted() {
                                let path = Some(MPath::join_opt_element(path.as_ref(), &name));
                                match entry {
                                    Entry::Leaf(leaf) => {
                                        output.push(OrderedTraversal::Output((
                                            path.clone(),
                                            Entry::Leaf(leaf),
                                        )));
                                    }
                                    Entry::Tree((weight, manifest_id)) => {
                                        output.push(OrderedTraversal::Recurse(
                                            weight,
                                            (manifest_id, Default::default(), path, true),
                                        ));
                                    }
                                }
                            }
                        } else {
                            if select.is_selected() {
                                output.push(OrderedTraversal::Output((
                                    path.clone(),
                                    Entry::Tree(manifest_id),
                                )));
                            }
                            for (name, selector) in subentries {
                                if let Some(entry) = manifest.lookup_weighted(&name) {
                                    let path = Some(MPath::join_opt_element(path.as_ref(), &name));
                                    match entry {
                                        Entry::Leaf(leaf) => {
                                            if selector.value.is_selected() {
                                                output.push(OrderedTraversal::Output((
                                                    path.clone(),
                                                    Entry::Leaf(leaf),
                                                )));
                                            }
                                        }
                                        Entry::Tree((weight, manifest_id)) => {
                                            output.push(OrderedTraversal::Recurse(
                                                weight,
                                                (manifest_id, selector, path, false),
                                            ));
                                        }
                                    }
                                }
                            }
                        }

                        Ok::<_, Error>(output)
                    }.boxed()
                },
            );

            pin_mut!(s);
            while let Some(value) = s.next().await {
                yield value;
            }
        })
        .boxed()
    }
}

impl<TreeId, Store> ManifestOrderedOps<Store> for TreeId
where
    Store: Sync + Send + Clone + 'static,
    Self: StoreLoadable<Store> + Clone + Send + Sync + Eq + Unpin + 'static,
    <Self as StoreLoadable<Store>>::Value: Manifest<TreeId = Self> + OrderedManifest + Send,
    <<Self as StoreLoadable<Store>>::Value as Manifest>::LeafId: Send + Clone + Eq + Unpin,
{
}
