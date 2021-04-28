/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use dag::Vertex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Dag(#[from] dag::Error),

    #[error("hash mismatch ({0:?} != {1:?})")]
    HashMismatch(Vertex, Vertex),
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Dag(dag::errors::BackendError::from(err).into())
    }
}

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        Self::Dag(dag::errors::BackendError::from(err).into())
    }
}

impl From<zstore::Error> for Error {
    fn from(err: zstore::Error) -> Self {
        anyhow::Error::from(err).into()
    }
}

impl From<metalog::Error> for Error {
    fn from(err: metalog::Error) -> Self {
        anyhow::Error::from(err).into()
    }
}
