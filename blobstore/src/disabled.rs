// Copyright (c) 2019-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use context::CoreContext;
use failure_ext::{format_err, Error};
use futures::future::IntoFuture;
use futures_ext::{BoxFuture, FutureExt};

use super::{Blobstore, BlobstoreBytes};

/// Disabled blobstore which fails all operations with a reason. Primarily used as a
/// placeholder for administratively disabled blobstores.
#[derive(Debug)]
pub struct DisabledBlob {
    reason: String,
}

impl DisabledBlob {
    pub fn new(reason: impl Into<String>) -> Self {
        DisabledBlob {
            reason: reason.into(),
        }
    }
}

impl Blobstore for DisabledBlob {
    fn get(&self, _ctx: CoreContext, _key: String) -> BoxFuture<Option<BlobstoreBytes>, Error> {
        Err(format_err!("Blobstore disabled: {}", self.reason))
            .into_future()
            .boxify()
    }

    fn put(&self, _ctx: CoreContext, _key: String, _value: BlobstoreBytes) -> BoxFuture<(), Error> {
        Err(format_err!("Blobstore disabled: {}", self.reason))
            .into_future()
            .boxify()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_disabled() {
        let disabled = DisabledBlob::new("test");
        let ctx = CoreContext::test_mock();

        let mut runtime = tokio::runtime::Runtime::new().unwrap();

        match runtime.block_on(disabled.get(ctx.clone(), "foobar".to_string())) {
            Ok(_) => panic!("Unexpected success"),
            Err(err) => println!("Got error: {:?}", err),
        }

        match runtime.block_on(disabled.put(
            ctx,
            "foobar".to_string(),
            BlobstoreBytes::from_bytes(vec![]),
        )) {
            Ok(_) => panic!("Unexpected success"),
            Err(err) => println!("Got error: {:?}", err),
        }
    }
}
