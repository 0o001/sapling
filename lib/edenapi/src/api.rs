// Copyright Facebook, Inc. 2019

use failure::Fallible;

use revisionstore::{MutableDeltaStore, MutableHistoryStore};
use types::Key;

use crate::progress::ProgressFn;

pub trait EdenApi: Send + Sync {
    /// Hit the API server's /health_check endpoint.
    /// Returns Ok(()) if the expected response is received, or an Error otherwise
    /// (e.g., if there was a connection problem or an unexpected repsonse).
    fn health_check(&self) -> Fallible<()>;

    /// Get the hostname of the API server.
    fn hostname(&self) -> Fallible<String>;

    /// Fetch the content of the specified files from the API server and write
    /// them to the store. Optionally takes a callback to report progress.
    ///
    /// Note that the keys are passed in as a `Vec` rather than using `IntoIterator`
    /// in order to keep this trait object-safe.
    fn get_files(
        &self,
        keys: Vec<Key>,
        store: &mut MutableDeltaStore,
        progress: Option<ProgressFn>,
    ) -> Fallible<()>;

    /// Fetch the history of the specified files from the API server and write
    /// them to the store.  Optionally takes a callback to report progress.
    ///
    /// Note that the keys are passed in as a `Vec` rather than using `IntoIterator`
    /// in order to keep this trait object-safe.
    fn get_history(
        &self,
        keys: Vec<Key>,
        store: &mut MutableHistoryStore,
        max_depth: Option<u32>,
        progress: Option<ProgressFn>,
    ) -> Fallible<()>;

    /// Fetch the specified trees from the API server and write them to the store.
    /// Optionally takes a callback to report progress.
    ///
    /// Note that the keys are passed in as a `Vec` rather than using `IntoIterator`
    /// in order to keep this trait object-safe.
    fn get_trees(
        &self,
        keys: Vec<Key>,
        store: &mut MutableDeltaStore,
        progress: Option<ProgressFn>,
    ) -> Fallible<()>;
}

// Statically ensure that the EdenApi trait is object safe using
// a dummy function that takes an EdenApi trait object.
//
// We want the trait to be object safe so that it is possible to
// dynamically choose between multiple implementations in the
// Python bindings.
fn _assert_object_safety(_: &EdenApi) {}
