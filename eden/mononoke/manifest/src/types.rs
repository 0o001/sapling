/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Result;
use async_trait::async_trait;
use blobstore::{Blobstore, Loadable, LoadableError, Storable};
use context::CoreContext;
use mononoke_types::{
    fsnode::{Fsnode, FsnodeEntry, FsnodeFile},
    skeleton_manifest::{SkeletonManifest, SkeletonManifestEntry},
    unode::{ManifestUnode, UnodeEntry},
    FileUnodeId, FsnodeId, MPath, MPathElement, ManifestUnodeId, SkeletonManifestId,
};
use serde_derive::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    iter::FromIterator,
};

pub trait Manifest: Sized + 'static {
    type TreeId;
    type LeafId;

    fn list(&self) -> Box<dyn Iterator<Item = (MPathElement, Entry<Self::TreeId, Self::LeafId>)>>;
    fn lookup(&self, name: &MPathElement) -> Option<Entry<Self::TreeId, Self::LeafId>>;
}

impl Manifest for ManifestUnode {
    type TreeId = ManifestUnodeId;
    type LeafId = FileUnodeId;

    fn lookup(&self, name: &MPathElement) -> Option<Entry<Self::TreeId, Self::LeafId>> {
        self.lookup(name).map(convert_unode)
    }

    fn list(&self) -> Box<dyn Iterator<Item = (MPathElement, Entry<Self::TreeId, Self::LeafId>)>> {
        let v: Vec<_> = self
            .list()
            .map(|(basename, entry)| (basename.clone(), convert_unode(entry)))
            .collect();
        Box::new(v.into_iter())
    }
}

fn convert_unode(unode_entry: &UnodeEntry) -> Entry<ManifestUnodeId, FileUnodeId> {
    match unode_entry {
        UnodeEntry::File(file_unode_id) => Entry::Leaf(file_unode_id.clone()),
        UnodeEntry::Directory(mf_unode_id) => Entry::Tree(mf_unode_id.clone()),
    }
}

impl Manifest for Fsnode {
    type TreeId = FsnodeId;
    type LeafId = FsnodeFile;

    fn lookup(&self, name: &MPathElement) -> Option<Entry<Self::TreeId, Self::LeafId>> {
        self.lookup(name).map(convert_fsnode)
    }

    fn list(&self) -> Box<dyn Iterator<Item = (MPathElement, Entry<Self::TreeId, Self::LeafId>)>> {
        let v: Vec<_> = self
            .list()
            .map(|(basename, entry)| (basename.clone(), convert_fsnode(entry)))
            .collect();
        Box::new(v.into_iter())
    }
}

fn convert_fsnode(fsnode_entry: &FsnodeEntry) -> Entry<FsnodeId, FsnodeFile> {
    match fsnode_entry {
        FsnodeEntry::File(fsnode_file) => Entry::Leaf(*fsnode_file),
        FsnodeEntry::Directory(fsnode_directory) => Entry::Tree(fsnode_directory.id().clone()),
    }
}

impl Manifest for SkeletonManifest {
    type TreeId = SkeletonManifestId;
    type LeafId = ();

    fn lookup(&self, name: &MPathElement) -> Option<Entry<Self::TreeId, Self::LeafId>> {
        self.lookup(name).map(convert_skeleton_manifest)
    }

    fn list(&self) -> Box<dyn Iterator<Item = (MPathElement, Entry<Self::TreeId, Self::LeafId>)>> {
        let v: Vec<_> = self
            .list()
            .map(|(basename, entry)| (basename.clone(), convert_skeleton_manifest(entry)))
            .collect();
        Box::new(v.into_iter())
    }
}

fn convert_skeleton_manifest(
    skeleton_entry: &SkeletonManifestEntry,
) -> Entry<SkeletonManifestId, ()> {
    match skeleton_entry {
        SkeletonManifestEntry::File => Entry::Leaf(()),
        SkeletonManifestEntry::Directory(skeleton_directory) => {
            Entry::Tree(skeleton_directory.id().clone())
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Entry<T, L> {
    Tree(T),
    Leaf(L),
}

impl<T, L> Entry<T, L> {
    pub fn into_tree(self) -> Option<T> {
        match self {
            Entry::Tree(tree) => Some(tree),
            _ => None,
        }
    }

    pub fn into_leaf(self) -> Option<L> {
        match self {
            Entry::Leaf(leaf) => Some(leaf),
            _ => None,
        }
    }
}

#[async_trait]
impl<T, L> Loadable for Entry<T, L>
where
    T: Loadable + Sync,
    L: Loadable + Sync,
{
    type Value = Entry<T::Value, L::Value>;

    async fn load<'a, B: Blobstore>(
        &'a self,
        ctx: CoreContext,
        blobstore: &'a B,
    ) -> Result<Self::Value, LoadableError> {
        Ok(match self {
            Entry::Tree(tree_id) => Entry::Tree(tree_id.load(ctx, blobstore).await?),
            Entry::Leaf(leaf_id) => Entry::Leaf(leaf_id.load(ctx, blobstore).await?),
        })
    }
}

#[async_trait]
impl<T, L> Storable for Entry<T, L>
where
    T: Storable + Send,
    L: Storable + Send,
{
    type Key = Entry<T::Key, L::Key>;

    async fn store<B: Blobstore>(self, ctx: CoreContext, blobstore: &B) -> Result<Self::Key> {
        Ok(match self {
            Entry::Tree(tree) => Entry::Tree(tree.store(ctx, blobstore).await?),
            Entry::Leaf(leaf) => Entry::Leaf(leaf.store(ctx, blobstore).await?),
        })
    }
}

pub struct PathTree<V> {
    pub value: V,
    pub subentries: BTreeMap<MPathElement, Self>,
}

impl<V> PathTree<V>
where
    V: Default,
{
    pub fn insert(&mut self, path: Option<MPath>, value: V) {
        let mut node = path.into_iter().flatten().fold(self, |node, element| {
            node.subentries
                .entry(element)
                .or_insert_with(Default::default)
        });
        node.value = value;
    }

    pub fn get(&self, path: Option<&MPath>) -> Option<&V> {
        let mut tree = self;
        for elem in path.into_iter().flatten() {
            match tree.subentries.get(elem) {
                Some(subtree) => tree = subtree,
                None => return None,
            }
        }
        Some(&tree.value)
    }
}

impl<V> Default for PathTree<V>
where
    V: Default,
{
    fn default() -> Self {
        Self {
            value: Default::default(),
            subentries: Default::default(),
        }
    }
}

impl<V> FromIterator<(MPath, V)> for PathTree<V>
where
    V: Default,
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = (MPath, V)>,
    {
        let mut tree: Self = Default::default();
        for (path, value) in iter {
            tree.insert(Some(path), value);
        }
        tree
    }
}

impl<V> FromIterator<(Option<MPath>, V)> for PathTree<V>
where
    V: Default,
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = (Option<MPath>, V)>,
    {
        let mut tree: Self = Default::default();
        for (path, value) in iter {
            tree.insert(path, value);
        }
        tree
    }
}

pub struct PathTreeIter<V> {
    frames: Vec<(Option<MPath>, PathTree<V>)>,
}

impl<V> Iterator for PathTreeIter<V> {
    type Item = (Option<MPath>, V);

    fn next(&mut self) -> Option<Self::Item> {
        let (path, PathTree { value, subentries }) = self.frames.pop()?;
        for (name, subentry) in subentries {
            self.frames.push((
                Some(MPath::join_opt_element(path.as_ref(), &name)),
                subentry,
            ));
        }
        Some((path, value))
    }
}

impl<V> IntoIterator for PathTree<V> {
    type Item = (Option<MPath>, V);
    type IntoIter = PathTreeIter<V>;

    fn into_iter(self) -> Self::IntoIter {
        PathTreeIter {
            frames: vec![(None, self)],
        }
    }
}

/// Traced allows you to trace a given parent through manifest derivation. For example, if you
/// assign ID 1 to a tree, then perform manifest derivation, then further entries you presented to
/// you that came from this parent will have the same ID.
#[derive(Debug)]
pub struct Traced<I, E>(Option<I>, E);

impl<I, E: Hash> Hash for Traced<I, E> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.1.hash(state);
    }
}

impl<I, E: PartialEq> PartialEq for Traced<I, E> {
    fn eq(&self, other: &Self) -> bool {
        self.1 == other.1
    }
}

impl<I, E: Eq> Eq for Traced<I, E> {}

impl<I: Copy, E: Copy> Copy for Traced<I, E> {}

impl<I: Clone, E: Clone> Clone for Traced<I, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), self.1.clone())
    }
}

impl<I, E> Traced<I, E> {
    pub fn generate(e: E) -> Self {
        Self(None, e)
    }

    pub fn assign(i: I, e: E) -> Self {
        Self(Some(i), e)
    }

    pub fn id(&self) -> Option<&I> {
        self.0.as_ref()
    }

    pub fn untraced(&self) -> &E {
        &self.1
    }

    pub fn into_untraced(self) -> E {
        self.1
    }
}

impl<I: Copy, E> Traced<I, E> {
    fn inherit_into_entry<TreeId, LeafId>(
        &self,
        e: Entry<TreeId, LeafId>,
    ) -> Entry<Traced<I, TreeId>, Traced<I, LeafId>> {
        match e {
            Entry::Tree(t) => Entry::Tree(Traced(self.0, t)),
            Entry::Leaf(l) => Entry::Leaf(Traced(self.0, l)),
        }
    }
}

impl<I, TreeId, LeafId> Into<Entry<TreeId, LeafId>>
    for Entry<Traced<I, TreeId>, Traced<I, LeafId>>
{
    fn into(self: Self) -> Entry<TreeId, LeafId> {
        match self {
            Entry::Tree(Traced(_, t)) => Entry::Tree(t),
            Entry::Leaf(Traced(_, l)) => Entry::Leaf(l),
        }
    }
}

impl<I: Copy + 'static, M: Manifest> Manifest for Traced<I, M> {
    type TreeId = Traced<I, <M as Manifest>::TreeId>;
    type LeafId = Traced<I, <M as Manifest>::LeafId>;

    fn list(&self) -> Box<dyn Iterator<Item = (MPathElement, Entry<Self::TreeId, Self::LeafId>)>> {
        Box::new(
            self.1
                .list()
                .map(|(path, entry)| (path, self.inherit_into_entry(entry)))
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }

    fn lookup(&self, name: &MPathElement) -> Option<Entry<Self::TreeId, Self::LeafId>> {
        self.1.lookup(name).map(|e| self.inherit_into_entry(e))
    }
}

#[async_trait]
impl<I: Clone + 'static + Send + Sync, M: Loadable + Send + Sync> Loadable for Traced<I, M> {
    type Value = Traced<I, <M as Loadable>::Value>;

    async fn load<'a, B: Blobstore>(
        &'a self,
        ctx: CoreContext,
        blobstore: &'a B,
    ) -> Result<Self::Value, LoadableError> {
        let id = self.0.clone();
        let v = self.1.load(ctx, blobstore).await?;
        Ok(Traced(id, v))
    }
}
