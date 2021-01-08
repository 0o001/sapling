/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use gotham::state::State;

use gotham_ext::middleware::{ClientIdentity, ScubaHandler};
use scuba_ext::MononokeScubaSampleBuilder;

use crate::handlers::HandlerInfo;
use crate::middleware::RequestContext;

#[derive(Copy, Clone, Debug)]
pub enum EdenApiScubaKey {
    Repo,
    Method,
    User,
    HandlerError,
}

impl AsRef<str> for EdenApiScubaKey {
    fn as_ref(&self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Method => "method",
            Self::User => "user",
            Self::HandlerError => "handler_error",
        }
    }
}

impl Into<String> for EdenApiScubaKey {
    fn into(self) -> String {
        self.as_ref().to_string()
    }
}

#[derive(Clone)]
pub struct EdenApiScubaHandler {
    request_context: Option<RequestContext>,
    handler_info: Option<HandlerInfo>,
    client_username: Option<String>,
}

impl ScubaHandler for EdenApiScubaHandler {
    fn from_state(state: &State) -> Self {
        Self {
            request_context: state.try_borrow::<RequestContext>().cloned(),
            handler_info: state.try_borrow::<HandlerInfo>().cloned(),
            client_username: state
                .try_borrow::<ClientIdentity>()
                .and_then(|id| id.username())
                .map(ToString::to_string),
        }
    }

    fn add_stats(self, scuba: &mut MononokeScubaSampleBuilder) {
        scuba.add_opt(EdenApiScubaKey::User, self.client_username);

        if let Some(info) = self.handler_info {
            scuba.add_opt(EdenApiScubaKey::Repo, info.repo.clone());
            scuba.add_opt(EdenApiScubaKey::Method, info.method.map(|m| m.to_string()));
        }

        if let Some(ctx) = self.request_context {
            ctx.ctx.perf_counters().insert_perf_counters(scuba);
            scuba.add_opt(EdenApiScubaKey::HandlerError, ctx.handler_error_msg);
        }
    }
}
