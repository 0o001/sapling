// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::fmt::{self, Display};

use failure::Error;
use futures::future::{self, Future};
use futures::stream::{self, Stream};

use mononoke_types::{MPath, MPathElement};

use blob::Blob;
use blobnode::Parents;
use futures_ext::{BoxFuture, BoxStream, FutureExt, StreamExt};
use nodehash::EntryId;

/// Interface for a manifest
///
/// A `Manifest` represents the mapping between a list of names and `Entry`s - ie,
/// functionally equivalent to a directory.
///
/// The name "Manifest" comes from Mercurial, where a single object represents the entire repo
/// namespace ("flat manifest"). But modern Mercurial and Mononoke use a distinct Manifest for
/// each directory ("tree manifest"). As a result, operations on a manifest are path element at
/// a time.
///
/// TODO: (jsgf) T25575327 lookup should just take a single element
pub trait Manifest: Send + 'static {
    /// Look up a specific entry in the Manifest by name
    ///
    /// If the name exists, return it as Some(entry). If it doesn't exist, return None.
    /// If it returns an error, it indicates something went wrong with the underlying
    /// infrastructure.
    fn lookup(&self, path: &MPath) -> BoxFuture<Option<Box<Entry + Sync>>, Error>;

    /// List all the entries in the Manifest.
    ///
    /// Entries are returned in canonical order.
    fn list(&self) -> BoxStream<Box<Entry + Sync>, Error>;

    /// Return self as a type-erased boxed trait (still needed as a trait method? T25577105)
    fn boxed(self) -> Box<Manifest + Sync>
    where
        Self: Sync + Sized,
    {
        Box::new(self)
    }
}

pub struct EmptyManifest;

impl Manifest for EmptyManifest {
    fn lookup(&self, _path: &MPath) -> BoxFuture<Option<Box<Entry + Sync>>, Error> {
        future::ok(None).boxify()
    }

    fn list(&self) -> BoxStream<Box<Entry + Sync>, Error> {
        stream::empty().boxify()
    }
}

pub struct BoxManifest<M>
where
    M: Manifest,
{
    manifest: M,
}

impl<M> BoxManifest<M>
where
    M: Manifest + Sync + Send + 'static,
{
    pub fn new(manifest: M) -> Box<Manifest + Sync> {
        let bm = BoxManifest { manifest };

        Box::new(bm)
    }
}

impl<M> Manifest for BoxManifest<M>
where
    M: Manifest + Sync + Send + 'static,
{
    fn lookup(&self, path: &MPath) -> BoxFuture<Option<Box<Entry + Sync>>, Error> {
        self.manifest
            .lookup(path)
            .map(move |oe| oe.map(|e| BoxEntry::new(e)))
            .boxify()
    }

    fn list(&self) -> BoxStream<Box<Entry + Sync>, Error> {
        self.manifest.list().map(move |e| BoxEntry::new(e)).boxify()
    }
}

impl Manifest for Box<Manifest + Sync> {
    fn lookup(&self, path: &MPath) -> BoxFuture<Option<Box<Entry + Sync>>, Error> {
        (**self).lookup(path)
    }

    fn list(&self) -> BoxStream<Box<Entry + Sync>, Error> {
        (**self).list()
    }
}

impl Manifest for Box<Manifest> {
    fn lookup(&self, path: &MPath) -> BoxFuture<Option<Box<Entry + Sync>>, Error> {
        (**self).lookup(path)
    }

    fn list(&self) -> BoxStream<Box<Entry + Sync>, Error> {
        (**self).list()
    }
}

/// Type of an Entry
///
/// File and Executable are identical - they both represent files containing arbitrary content.
/// The only difference is that the Executables are created with executable permission when
/// checked out.
///
/// Symlink is also the same as File, but the content of the file is interpolated into a path
/// being traversed during lookup.
///
/// Tree is a reference to another Manifest (directory-like) object.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize)]
pub enum Type {
    File,
    Symlink,
    Tree,
    Executable,
}

impl Display for Type {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            &Type::Symlink => "l",
            &Type::Executable => "x",
            &Type::Tree => "t",
            &Type::File => "",
        };
        write!(fmt, "{}", s)
    }
}

/// Concrete representation of various Entry Types.
pub enum Content {
    File(Blob),       // TODO stream
    Executable(Blob), // TODO stream
    Symlink(MPath),
    Tree(Box<Manifest + Sync>),
}

/// An entry represents a single entry in a Manifest
///
/// The Entry has at least a name, a type, and the identity of the object it refers to

pub trait Entry: Send + 'static {
    /// Type of the object this entry refers to
    fn get_type(&self) -> Type;

    /// Get the parents (in the history graph) of the referred-to object
    fn get_parents(&self) -> BoxFuture<Parents, Error>;

    /// Get the raw content of the object as it exists in the blobstore,
    /// without any interpretation. This is only really useful for doing a bit-level duplication.
    fn get_raw_content(&self) -> BoxFuture<Blob, Error>;

    /// Get the interpreted content of the object. This will likely require IO
    fn get_content(&self) -> BoxFuture<Content, Error>;

    /// Get the logical size of the entry. Some entries don't really have a meaningful size.
    fn get_size(&self) -> BoxFuture<Option<usize>, Error>;

    /// Get the identity of the object this entry refers to.
    fn get_hash(&self) -> &EntryId;

    /// Get the name of the entry. None means that this is a root entry
    fn get_name(&self) -> Option<&MPathElement>;

    /// Return an Entry as a type-erased trait object.
    /// (Do we still need this as a trait method? T25577105)
    fn boxed(self) -> Box<Entry + Sync>
    where
        Self: Sync + Sized,
    {
        Box::new(self)
    }
}

/// Wrapper for boxing an instance of Entry
///
/// TODO: (jsgf) T25577105 Are the Box variants of Manifest/Entry traits still needed?
pub struct BoxEntry<Ent>
where
    Ent: Entry,
{
    entry: Ent,
}

impl<Ent> BoxEntry<Ent>
where
    Ent: Entry + Sync + Send + 'static,
{
    pub fn new(entry: Ent) -> Box<Entry + Sync> {
        Box::new(BoxEntry { entry })
    }
}

impl<Ent> Entry for BoxEntry<Ent>
where
    Ent: Entry + Sync + Send + 'static,
{
    fn get_type(&self) -> Type {
        self.entry.get_type()
    }

    fn get_parents(&self) -> BoxFuture<Parents, Error> {
        self.entry.get_parents().boxify()
    }

    fn get_raw_content(&self) -> BoxFuture<Blob, Error> {
        self.entry.get_raw_content().boxify()
    }

    fn get_content(&self) -> BoxFuture<Content, Error> {
        self.entry.get_content().boxify()
    }

    fn get_size(&self) -> BoxFuture<Option<usize>, Error> {
        self.entry.get_size().boxify()
    }

    fn get_hash(&self) -> &EntryId {
        self.entry.get_hash()
    }

    fn get_name(&self) -> Option<&MPathElement> {
        self.entry.get_name()
    }
}

impl Entry for Box<Entry + Sync> {
    fn get_type(&self) -> Type {
        (**self).get_type()
    }

    fn get_parents(&self) -> BoxFuture<Parents, Error> {
        (**self).get_parents()
    }

    fn get_raw_content(&self) -> BoxFuture<Blob, Error> {
        (**self).get_raw_content()
    }

    fn get_content(&self) -> BoxFuture<Content, Error> {
        (**self).get_content()
    }

    fn get_size(&self) -> BoxFuture<Option<usize>, Error> {
        (**self).get_size()
    }

    fn get_hash(&self) -> &EntryId {
        (**self).get_hash()
    }

    fn get_name(&self) -> Option<&MPathElement> {
        (**self).get_name()
    }
}
