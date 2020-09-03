/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::panic::RefUnwindSafe;

use gotham::state::State;
use hyper::{Body, Response};

pub mod client_identity;
pub mod post_request;
pub mod server_identity;
pub mod timer;
pub mod tls_session_data;

pub use client_identity::{ClientIdentity, ClientIdentityMiddleware};
pub use post_request::{PostRequestCallbacks, PostRequestConfig, PostRequestMiddleware};
pub use server_identity::ServerIdentityMiddleware;
pub use timer::{HeadersDuration, RequestStartTime, TimerMiddleware};
pub use tls_session_data::TlsSessionDataMiddleware;

#[async_trait::async_trait]
pub trait Middleware: 'static + RefUnwindSafe + Send + Sync {
    async fn inbound(&self, _state: &mut State) -> Option<Response<Body>> {
        // Implement inbound to perform pre-request actions, such as putting something in the
        // state.

        None
    }

    async fn outbound(&self, _state: &mut State, _response: &mut Response<Body>) {
        // Implement outbound to perform post-request actions, such as logging the response status
        // code.
    }
}
