/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::num::NonZeroU64;

use anyhow::{format_err, Context, Error};
use async_trait::async_trait;
use bytes::Bytes;
use context::PerfCounterType;
use futures::{stream, Stream, StreamExt, TryStreamExt};
use gotham::state::{FromState, State};
use gotham_derive::{StateData, StaticResponseExtender};
use hyper::Body;
use serde::Deserialize;
use std::str::FromStr;

use edenapi_types::{
    wire::ToWire, AnyFileContentId, AnyId, Batch, FileAttributes, FileAuxData, FileContent,
    FileContentTokenMetadata, FileEntry, FileRequest, FileSpec, UploadHgFilenodeRequest,
    UploadToken, UploadTokenMetadata, UploadTokensResponse,
};
use ephemeral_blobstore::BubbleId;
use gotham_ext::{error::HttpError, response::TryIntoResponse};
use mercurial_types::{HgFileNodeId, HgNodeHash};
use mononoke_api_hg::{HgDataContext, HgDataId, HgRepoContext};
use mononoke_types::{hash::Sha1, hash::Sha256, ContentId};
use rate_limiting::Metric;
use types::Key;

use crate::context::ServerContext;
use crate::errors::ErrorKind;
use crate::middleware::RequestContext;
use crate::utils::{cbor_stream_filtered_errors, get_repo};

use super::{EdenApiHandler, EdenApiMethod, HandlerInfo, HandlerResult};

/// XXX: This number was chosen arbitrarily.
const MAX_CONCURRENT_FILE_FETCHES_PER_REQUEST: usize = 10;

const MAX_CONCURRENT_UPLOAD_FILENODES_PER_REQUEST: usize = 1000;

#[derive(Debug, Deserialize, StateData, StaticResponseExtender)]
pub struct UploadFileParams {
    repo: String,
    idtype: String,
    id: String,
}

#[derive(Debug, Deserialize, StateData, StaticResponseExtender)]
pub struct UploadFileQueryString {
    bubble_id: Option<NonZeroU64>,
    content_size: u64,
}

/// Fetch the content of the files requested by the client.
pub struct FilesHandler;

#[async_trait]
impl EdenApiHandler for FilesHandler {
    type Request = FileRequest;
    type Response = FileEntry;

    const HTTP_METHOD: hyper::Method = hyper::Method::POST;
    const API_METHOD: EdenApiMethod = EdenApiMethod::Files;
    const ENDPOINT: &'static str = "/files";

    async fn handler(
        repo: HgRepoContext,
        _path: Self::PathExtractor,
        _query: Self::QueryStringExtractor,
        request: Self::Request,
    ) -> HandlerResult<'async_trait, Self::Response> {
        let ctx = repo.ctx().clone();

        let len = request.keys.len() + request.reqs.len();
        let reqs = request
            .keys
            .into_iter()
            .map(|key| FileSpec {
                key,
                attrs: FileAttributes {
                    content: true,
                    aux_data: false,
                },
            })
            .chain(request.reqs.into_iter());
        ctx.perf_counters()
            .add_to_counter(PerfCounterType::EdenapiFiles, len as i64);
        let fetches = reqs.map(move |FileSpec { key, attrs }| fetch_file(repo.clone(), key, attrs));

        Ok(stream::iter(fetches)
            .buffer_unordered(MAX_CONCURRENT_FILE_FETCHES_PER_REQUEST)
            .inspect_ok(move |_| {
                ctx.session().bump_load(Metric::GetpackFiles, 1.0);
            })
            .boxed())
    }
}

/// Fetch requested file for a single key.
/// Note that this function consumes the repo context in order
/// to construct a file context for the requested blob.
async fn fetch_file(
    repo: HgRepoContext,
    key: Key,
    attrs: FileAttributes,
) -> Result<FileEntry, Error> {
    let id = HgFileNodeId::from_node_hash(HgNodeHash::from(key.hgid));

    let ctx = id
        .context(repo)
        .await
        .with_context(|| ErrorKind::FileFetchFailed(key.clone()))?
        .with_context(|| ErrorKind::KeyDoesNotExist(key.clone()))?;

    let parents = ctx.hg_parents().into();
    let mut file = FileEntry::new(key.clone(), parents);

    if attrs.content {
        let (data, metadata) = ctx
            .content()
            .await
            .with_context(|| ErrorKind::FileFetchFailed(key.clone()))?;

        file = file.with_content(FileContent {
            hg_file_blob: data,
            metadata,
        });
    }

    if attrs.aux_data {
        let content_metadata = ctx
            .content_metadata()
            .await
            .with_context(|| ErrorKind::FileFetchFailed(key.clone()))?;

        file = file.with_aux_data(FileAuxData {
            total_size: content_metadata.total_size,
            content_id: content_metadata.content_id.into(),
            sha1: content_metadata.sha1.into(),
            sha256: content_metadata.sha256.into(),
        });
    }

    Ok(file)
}

/// Generate an upload token for alredy uploaded content
async fn generate_upload_token(
    _repo: HgRepoContext,
    id: AnyFileContentId,
    content_size: u64,
    bubble_id: Option<NonZeroU64>,
) -> Result<UploadToken, Error> {
    // At first, returns a fake token
    Ok(UploadToken::new_fake_token_with_metadata(
        AnyId::AnyFileContentId(id),
        bubble_id,
        UploadTokenMetadata::FileContentTokenMetadata(FileContentTokenMetadata { content_size }),
    ))
}

/// Upload content of a file
async fn store_file(
    repo: HgRepoContext,
    id: AnyFileContentId,
    data: impl Stream<Item = Result<Bytes, Error>> + Send,
    content_size: u64,
    bubble_id: Option<BubbleId>,
) -> Result<(), Error> {
    match id {
        AnyFileContentId::ContentId(id) => {
            repo.store_file_by_contentid(ContentId::from(id), content_size, data, bubble_id)
                .await?
        }
        AnyFileContentId::Sha1(id) => {
            repo.store_file_by_sha1(Sha1::from(id), content_size, data, bubble_id)
                .await?
        }
        AnyFileContentId::Sha256(id) => {
            repo.store_file_by_sha256(Sha256::from(id), content_size, data, bubble_id)
                .await?
        }
    };
    Ok(())
}

/// Upload content of a file requested by the client.
pub async fn upload_file(state: &mut State) -> Result<impl TryIntoResponse, HttpError> {
    let params = UploadFileParams::take_from(state);
    let query_string = UploadFileQueryString::take_from(state);

    state.put(HandlerInfo::new(&params.repo, EdenApiMethod::UploadFile));

    let rctx = RequestContext::borrow_from(state).clone();
    let sctx = ServerContext::borrow_from(state);

    let repo = get_repo(&sctx, &rctx, &params.repo, None).await?;

    let id = AnyFileContentId::from_str(&format!("{}/{}", &params.idtype, &params.id))
        .map_err(HttpError::e400)?;

    let body = Body::take_from(state).map_err(Error::from);
    let content_size = query_string.content_size;

    store_file(
        repo.clone(),
        id.clone(),
        body,
        content_size,
        query_string.bubble_id.map(BubbleId::new),
    )
    .await
    .map_err(HttpError::e500)?;

    let token = generate_upload_token(repo, id, content_size, query_string.bubble_id)
        .await
        .map(|v| v.to_wire());

    Ok(cbor_stream_filtered_errors(stream::iter(vec![token])))
}

/// Store the content of a single HgFilenode
async fn store_hg_filenode(
    repo: HgRepoContext,
    item: UploadHgFilenodeRequest,
    index: usize,
) -> Result<UploadTokensResponse, Error> {
    // TODO(liubovd): validate signature of the upload token (item.token) and
    // return 'ErrorKind::UploadHgFilenodeRequestInvalidToken' if it's invalid.
    // This will be added later, for now assume tokens are always valid.

    let node_id = item.data.node_id;
    let token = item.data.file_content_upload_token;

    let filenode: HgFileNodeId = HgFileNodeId::from_node_hash(HgNodeHash::from(node_id));

    let p1: Option<HgFileNodeId> = item
        .data
        .parents
        .p1()
        .cloned()
        .map(HgNodeHash::from)
        .map(HgFileNodeId::from_node_hash);

    let p2: Option<HgFileNodeId> = item
        .data
        .parents
        .p2()
        .cloned()
        .map(HgNodeHash::from)
        .map(HgFileNodeId::from_node_hash);

    let any_file_content_id = match token.data.id {
        AnyId::AnyFileContentId(id) => Some(id),
        _ => None,
    }
    .ok_or_else(|| {
        ErrorKind::UploadHgFilenodeRequestInvalidToken(
            node_id.clone(),
            "the provided token is not for file content".into(),
        )
    })?;

    let content_id = repo
        .convert_file_to_content_id(any_file_content_id, None)
        .await?
        .ok_or_else(|| format_err!("File from upload token should be present"))?;

    let content_size = match token.data.metadata {
        Some(UploadTokenMetadata::FileContentTokenMetadata(meta)) => meta.content_size,
        _ => repo.fetch_file_content_size(content_id, None).await?,
    };

    let metadata = Bytes::from(item.data.metadata);

    repo.store_hg_filenode(filenode, p1, p2, content_id, content_size, metadata)
        .await?;

    Ok(UploadTokensResponse {
        index,
        token: UploadToken::new_fake_token(AnyId::HgFilenodeId(node_id), None),
    })
}

/// Upload list of hg filenodes requested by the client (batch request).
pub struct UploadHgFilenodesHandler;

#[async_trait]
impl EdenApiHandler for UploadHgFilenodesHandler {
    type Request = Batch<UploadHgFilenodeRequest>;
    type Response = UploadTokensResponse;

    const HTTP_METHOD: hyper::Method = hyper::Method::POST;
    const API_METHOD: EdenApiMethod = EdenApiMethod::UploadHgFilenodes;
    const ENDPOINT: &'static str = "/upload/filenodes";

    async fn handler(
        repo: HgRepoContext,
        _path: Self::PathExtractor,
        _query: Self::QueryStringExtractor,
        request: Self::Request,
    ) -> HandlerResult<'async_trait, Self::Response> {
        let tokens = request
            .batch
            .into_iter()
            .enumerate()
            .map(move |(i, item)| store_hg_filenode(repo.clone(), item, i));
        Ok(stream::iter(tokens)
            .buffer_unordered(MAX_CONCURRENT_UPLOAD_FILENODES_PER_REQUEST)
            .boxed())
    }
}

/// Downloads a file given an upload token
pub struct DownloadFileHandler;

#[async_trait]
impl EdenApiHandler for DownloadFileHandler {
    type Request = UploadToken;
    type Response = Bytes;

    const HTTP_METHOD: hyper::Method = hyper::Method::POST;
    const API_METHOD: EdenApiMethod = EdenApiMethod::DownloadFile;
    const ENDPOINT: &'static str = "/download/file";

    async fn handler(
        repo: HgRepoContext,
        _path: Self::PathExtractor,
        _query: Self::QueryStringExtractor,
        request: Self::Request,
    ) -> HandlerResult<'async_trait, Self::Response> {
        let content = repo
            .download_file(request)
            .await?
            .context("File not found")?;
        Ok(content.boxed())
    }
}
