// Copyright (c) 2017-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use futures::future::Future;
use futures::stream::Stream;
use mercurial_types::{NodeHash, Repo};
use repoinfo::{Generation, RepoGenCache};
use std::boxed::Box;
use std::sync::Arc;

use NodeStream;
use errors::*;

use futures::{Async, Poll};

pub type InputStream = Box<Stream<Item = (NodeHash, Generation), Error = Error> + 'static + Send>;

pub fn add_generations<R>(
    stream: Box<NodeStream>,
    repo_generation: RepoGenCache<R>,
    repo: Arc<R>,
) -> InputStream
where
    R: Repo,
{
    let stream = stream.and_then(move |node_hash| {
        repo_generation
            .get(&repo, node_hash)
            .map(move |gen_id| (node_hash, gen_id))
            .map_err(|err| err.context(ErrorKind::GenerationFetchFailed))
            .from_err()
    });
    Box::new(stream)
}

pub fn all_inputs_ready(
    inputs: &Vec<(InputStream, Poll<Option<(NodeHash, Generation)>, Error>)>,
) -> bool {
    inputs
        .iter()
        .map(|&(_, ref state)| match state {
            &Err(_) => false,
            &Ok(ref p) => p.is_ready(),
        })
        .all(|ready| ready)
}

pub fn poll_all_inputs(
    inputs: &mut Vec<(InputStream, Poll<Option<(NodeHash, Generation)>, Error>)>,
) {
    for &mut (ref mut input, ref mut state) in inputs.iter_mut() {
        if let Ok(Async::NotReady) = *state {
            *state = input.poll();
        }
    }
}

#[cfg(test)]
pub struct NotReadyEmptyStream {
    pub poll_count: usize,
}

#[cfg(test)]
impl Stream for NotReadyEmptyStream {
    type Item = NodeHash;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        if self.poll_count == 0 {
            Ok(Async::Ready(None))
        } else {
            self.poll_count -= 1;
            Ok(Async::NotReady)
        }
    }
}

#[cfg(test)]
pub struct RepoErrorStream {
    pub hash: NodeHash,
}

#[cfg(test)]
impl Stream for RepoErrorStream {
    type Item = NodeHash;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        Err(ErrorKind::RepoError(self.hash))?;
        unreachable!()
    }
}
