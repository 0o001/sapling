/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::fmt;
use std::pin::Pin;

use anyhow::{Context, Error};
use edenapi_types::ToWire;
use futures::{stream::TryStreamExt, FutureExt};
use gotham::{
    handler::{HandlerError as GothamHandlerError, HandlerFuture},
    middleware::state::StateMiddleware,
    pipeline::{new_pipeline, single::single_pipeline},
    router::{
        builder::RouterBuilder,
        builder::{build_router as gotham_build_router, DefineSingleRoute, DrawRoutes},
        Router,
    },
    state::{request_id, FromState, State},
};
use gotham_derive::StateData;
use gotham_ext::{
    error::{ErrorFormatter, HttpError},
    response::build_response,
};
use hyper::{Body, Response};
use mime::Mime;
use serde::{Deserialize, Serialize};

use crate::context::ServerContext;
use crate::middleware::RequestContext;
use crate::utils::{cbor_stream_filtered_errors, get_repo, parse_wire_request};

mod bookmarks;
mod clone;
mod commit;
mod complete_trees;
mod files;
mod handler;
mod history;
mod lookup;
mod pull;
mod repos;
mod trees;

pub(crate) use handler::{EdenApiHandler, HandlerError, HandlerResult, PathExtractorWithRepo};

/// Enum identifying the EdenAPI method that each handler corresponds to.
/// Used to identify the handler for logging and stats collection.
#[derive(Copy, Clone)]
pub enum EdenApiMethod {
    Files,
    Lookup,
    UploadFile,
    UploadHgFilenodes,
    UploadTrees,
    UploadHgChangesets,
    UploadBonsaiChangeset,
    Trees,
    CompleteTrees,
    History,
    CommitLocationToHash,
    CommitHashToLocation,
    CommitRevlogData,
    CommitHashLookup,
    Clone,
    FullIdMapClone,
    Bookmarks,
    PullFastForwardMaster,
    EphemeralPrepare,
    FetchSnapshot,
}

impl fmt::Display for EdenApiMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Files => "files",
            Self::Trees => "trees",
            Self::CompleteTrees => "complete_trees",
            Self::History => "history",
            Self::CommitLocationToHash => "commit_location_to_hash",
            Self::CommitHashToLocation => "commit_hash_to_location",
            Self::CommitRevlogData => "commit_revlog_data",
            Self::CommitHashLookup => "commit_hash_lookup",
            Self::Clone => "clone",
            Self::FullIdMapClone => "full_idmap_clone",
            Self::Bookmarks => "bookmarks",
            Self::Lookup => "lookup",
            Self::UploadFile => "upload_file",
            Self::PullFastForwardMaster => "pull_fast_forward_master",
            Self::UploadHgFilenodes => "upload_filenodes",
            Self::UploadTrees => "upload_trees",
            Self::UploadHgChangesets => "upload_hg_changesets",
            Self::UploadBonsaiChangeset => "upload_bonsai_changeset",
            Self::EphemeralPrepare => "ephemeral_prepare",
            Self::FetchSnapshot => "fetch_snapshot",
        };
        write!(f, "{}", name)
    }
}

/// Information about the handler that served the request.
///
/// This should be inserted into the request's `State` by each handler. It will
/// typically be used by middlware for request logging and stats reporting.
#[derive(Default, StateData, Clone)]
pub struct HandlerInfo {
    pub repo: Option<String>,
    pub method: Option<EdenApiMethod>,
}

impl HandlerInfo {
    pub fn new(repo: impl ToString, method: EdenApiMethod) -> Self {
        Self {
            repo: Some(repo.to_string()),
            method: Some(method),
        }
    }
}

/// JSON representation of an error to send to the client.
#[derive(Clone, Serialize, Debug, Deserialize)]
struct JsonError {
    message: String,
    request_id: String,
}

struct JsonErrorFomatter;

impl ErrorFormatter for JsonErrorFomatter {
    type Body = Vec<u8>;

    fn format(&self, error: &Error, state: &State) -> Result<(Self::Body, Mime), Error> {
        let message = format!("{:#}", error);

        // Package the error message into a JSON response.
        let res = JsonError {
            message,
            request_id: request_id(&state).to_string(),
        };

        let body = serde_json::to_vec(&res).context("Failed to serialize error")?;

        Ok((body, mime::APPLICATION_JSON))
    }
}

/// Macro to create a Gotham handler function from an async function.
///
/// The expected signature of the input function is:
/// ```rust,ignore
/// async fn handler(state: &mut State) -> Result<impl TryIntoResponse, HttpError>
/// ```
///
/// The resulting wrapped function will have the signaure:
/// ```rust,ignore
/// fn wrapped(mut state: State) -> Pin<Box<HandlerFuture>>
/// ```
macro_rules! define_handler {
    ($name:ident, $func:path) => {
        fn $name(mut state: State) -> Pin<Box<HandlerFuture>> {
            async move {
                let res = $func(&mut state).await;
                build_response(res, state, &JsonErrorFomatter)
            }
            .boxed()
        }
    };
}

define_handler!(repos_handler, repos::repos);
define_handler!(trees_handler, trees::trees);
define_handler!(commit_hash_to_location_handler, commit::hash_to_location);
define_handler!(commit_revlog_data_handler, commit::revlog_data);
define_handler!(clone_handler, clone::clone_data);
define_handler!(full_idmap_clone_handler, clone::full_idmap_clone_data);
define_handler!(upload_file_handler, files::upload_file);
define_handler!(pull_fast_forward_master, pull::pull_fast_forward_master);

fn health_handler(state: State) -> (State, &'static str) {
    if ServerContext::borrow_from(&state).will_exit() {
        (state, "EXITING")
    } else {
        (state, "I_AM_ALIVE")
    }
}

async fn handler_wrapper<Handler: EdenApiHandler>(
    mut state: State,
) -> Result<(State, Response<Body>), (State, GothamHandlerError)> {
    let res = async {
        let path = Handler::PathExtractor::take_from(&mut state);
        let query_string = Handler::QueryStringExtractor::take_from(&mut state);

        state.put(HandlerInfo::new(path.repo(), Handler::API_METHOD));

        let rctx = RequestContext::borrow_from(&mut state).clone();
        let sctx = ServerContext::borrow_from(&mut state);

        let repo = get_repo(&sctx, &rctx, path.repo(), None).await?;
        let request = parse_wire_request::<<Handler::Request as ToWire>::Wire>(&mut state).await?;
        match Handler::handler(repo, path, query_string, request).await {
            Ok(responses) => Ok(cbor_stream_filtered_errors(
                responses.map_ok(ToWire::to_wire),
            )),
            Err(HandlerError::E500(err)) => Err(HttpError::e500(err)),
            Err(HandlerError::E400(err)) => Err(HttpError::e400(err)),
        }
    }
    .await;

    build_response(res, state, &JsonErrorFomatter)
}

// We use a struct here (rather than just a global function) just for the convenience
// of writing `Handlers::setup::<MyHandler>(route)`
// instead of `setup_handler::<MyHandler, _, _>(route)`, to make things clearer.
struct Handlers<C, P> {
    _phantom: (std::marker::PhantomData<C>, std::marker::PhantomData<P>),
}

impl<C, P> Handlers<C, P>
where
    C: gotham::pipeline::chain::PipelineHandleChain<P> + Copy + Send + Sync + 'static,
    P: std::panic::RefUnwindSafe + Send + Sync + 'static,
{
    fn setup<Handler: EdenApiHandler>(route: &mut RouterBuilder<C, P>) {
        route
            .request(
                vec![Handler::HTTP_METHOD],
                &format!("/:repo{}", Handler::ENDPOINT),
            )
            .with_path_extractor::<Handler::PathExtractor>()
            .with_query_string_extractor::<Handler::QueryStringExtractor>()
            .to_async(handler_wrapper::<Handler>);
    }
}

pub fn build_router(ctx: ServerContext) -> Router {
    let pipeline = new_pipeline().add(StateMiddleware::new(ctx)).build();
    let (chain, pipelines) = single_pipeline(pipeline);

    gotham_build_router(chain, pipelines, |route| {
        route.get("/health_check").to(health_handler);
        route.get("/repos").to(repos_handler);
        Handlers::setup::<commit::EphemeralPrepareHandler>(route);
        Handlers::setup::<commit::UploadHgChangesetsHandler>(route);
        Handlers::setup::<commit::UploadBonsaiChangesetHandler>(route);
        Handlers::setup::<commit::LocationToHashHandler>(route);
        Handlers::setup::<commit::HashLookupHandler>(route);
        Handlers::setup::<files::FilesHandler>(route);
        Handlers::setup::<files::UploadHgFilenodesHandler>(route);
        Handlers::setup::<bookmarks::BookmarksHandler>(route);
        Handlers::setup::<complete_trees::CompleteTreesHandler>(route);
        Handlers::setup::<history::HistoryHandler>(route);
        Handlers::setup::<lookup::LookupHandler>(route);
        Handlers::setup::<trees::UploadTreesHandler>(route);
        Handlers::setup::<commit::FetchSnapshotHandler>(route);
        route
            .post("/:repo/trees")
            .with_path_extractor::<trees::TreeParams>()
            .to(trees_handler);
        route
            .post("/:repo/commit/hash_to_location")
            .with_path_extractor::<commit::HashToLocationParams>()
            .to(commit_hash_to_location_handler);
        route
            .post("/:repo/commit/revlog_data")
            .with_path_extractor::<commit::RevlogDataParams>()
            .to(commit_revlog_data_handler);
        route
            .post("/:repo/clone")
            .with_path_extractor::<clone::CloneParams>()
            .to(clone_handler);
        route
            .post("/:repo/pull_fast_forward_master")
            .with_path_extractor::<pull::PullFastForwardParams>()
            .to(pull_fast_forward_master);
        route
            .post("/:repo/full_idmap_clone")
            .with_path_extractor::<clone::CloneParams>()
            .to(full_idmap_clone_handler);
        route
            .put("/:repo/upload/file/:idtype/:id")
            .with_path_extractor::<files::UploadFileParams>()
            .with_query_string_extractor::<files::UploadFileQueryString>()
            .to(upload_file_handler);
    })
}
