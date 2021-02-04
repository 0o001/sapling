/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::blobstore;
use crate::checkpoint::{CheckpointsByName, SqlCheckpoints};
use crate::graph::{EdgeType, Node, NodeType};
use crate::log;
use crate::parse_node::parse_node;
use crate::progress::{
    sort_by_string, ProgressOptions, ProgressStateCountByType, ProgressStateMutex, ProgressSummary,
};
use crate::sampling::SamplingOptions;
use crate::state::{InternedType, StepStats};
use crate::tail::TailParams;
use crate::validate::{CheckType, REPO, WALK_TYPE};
use crate::walk::{OutgoingEdge, RepoWalkParams};

use ::blobstore::Blobstore;
use anyhow::{bail, format_err, Context, Error};
use blobrepo_factory::{open_blobrepo_given_datasources, Caching, ReadOnlyStorage};
use blobstore_factory::{
    make_metadata_sql_factory, CachelibBlobstoreOptions, MetadataSqlFactory, ScrubAction,
};
use bookmarks::BookmarkName;
use clap::{App, Arg, ArgMatches, SubCommand, Values};
use cmdlib::args::{
    self, CachelibSettings, MononokeClapApp, MononokeMatches, RepoRequirement, ResolvedRepo,
};
use derived_data::BonsaiDerivable;
use derived_data_filenodes::FilenodesOnlyPublic;
use fbinit::FacebookInit;
use futures::compat::Future01CompatExt;
use itertools::{process_results, Itertools};
use maplit::hashset;
use mercurial_derived_data::MappedHgChangesetId;
use metaconfig_types::{BlobConfig, CensoredScubaParams, MetadataDatabaseConfig, Redaction};
use multiplexedblob::{ScrubHandler, ScrubOptions};
use once_cell::sync::Lazy;
use samplingblob::SamplingHandler;
use scuba_ext::MononokeScubaSampleBuilder;
use slog::{info, o, warn, Logger};
use sql_construct::{SqlConstruct, SqlConstructFromMetadataDatabaseConfig};
use sql_ext::facebook::MysqlOptions;
use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    iter::FromIterator,
    num::NonZeroU32,
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use strum::{IntoEnumIterator, VariantNames};
use strum_macros::{AsRefStr, EnumString, EnumVariantNames};

// Per repo things we don't pass into the walk
pub struct RepoSubcommandParams {
    pub progress_state: ProgressStateMutex<ProgressStateCountByType<StepStats, ProgressSummary>>,
    pub tail_params: TailParams,
}

// These don't vary per repo
#[derive(Clone)]
pub struct JobWalkParams {
    pub enable_derive: bool,
    pub quiet: bool,
    pub error_as_data_node_types: HashSet<NodeType>,
    pub error_as_data_edge_types: HashSet<EdgeType>,
    pub repo_count: usize,
}

const PROGRESS_SAMPLE_RATE: u64 = 1000;
const PROGRESS_SAMPLE_DURATION_S: u64 = 5;

// Sub commands
pub const SCRUB: &str = "scrub";
pub const COMPRESSION_BENEFIT: &str = "compression-benefit";
pub const VALIDATE: &str = "validate";
pub const CORPUS: &str = "corpus";

// Subcommand args
const QUIET_ARG: &str = "quiet";
const ENABLE_REDACTION_ARG: &str = "enable-redaction";
const SCHEDULED_MAX_ARG: &str = "scheduled-max";
const TAIL_INTERVAL_ARG: &str = "tail-interval";
const ERROR_AS_DATA_NODE_TYPE_ARG: &str = "error-as-data-node-type";
const ERROR_AS_DATA_EDGE_TYPE_ARG: &str = "error-as-data-edge-type";
const EXCLUDE_NODE_TYPE_ARG: &str = "exclude-node-type";
const INCLUDE_NODE_TYPE_ARG: &str = "include-node-type";
const EXCLUDE_EDGE_TYPE_ARG: &str = "exclude-edge-type";
const INCLUDE_EDGE_TYPE_ARG: &str = "include-edge-type";
const BOOKMARK_ARG: &str = "bookmark";
const WALK_ROOT_ARG: &str = "walk-root";
const CHUNK_BY_PUBLIC_ARG: &str = "chunk-by-public";
const CHUNK_SIZE_ARG: &str = "chunk-size";
const INCLUDE_CHUNK_CLEAR_INTERNED_TYPE_ARG: &str = "include-chunk-clear-interned-type";
const EXCLUDE_CHUNK_CLEAR_INTERNED_TYPE_ARG: &str = "exclude-chunk-clear-interned-type";
const INCLUDE_CHUNK_CLEAR_NODE_TYPE_ARG: &str = "include-chunk-clear-node-type";
const EXCLUDE_CHUNK_CLEAR_NODE_TYPE_ARG: &str = "exclude-chunk-clear-node-type";
const CHUNK_CLEAR_SAMPLE_RATE_ARG: &str = "chunk-clear-sample-rate";
const CHECKPOINT_NAME_ARG: &str = "checkpoint-name";
const CHECKPOINT_PATH_ARG: &str = "checkpoint-path";
const CHECKPOINT_SAMPLE_RATE_ARG: &str = "checkpoint-sample-rate";
const STATE_MAX_AGE_ARG: &str = "state-max-age";
const ALLOW_REMAINING_DEFERRED_ARG: &str = "allow-remaining-deferred";
const INNER_BLOBSTORE_ID_ARG: &str = "inner-blobstore-id";
const SCRUB_BLOBSTORE_ACTION_ARG: &str = "scrub-blobstore-action";
const ENABLE_DERIVE_ARG: &str = "enable-derive";
const PROGRESS_SAMPLE_RATE_ARG: &str = "progress-sample-rate";
const PROGRESS_INTERVAL_ARG: &str = "progress-interval";
pub const LIMIT_DATA_FETCH_ARG: &str = "limit-data-fetch";
pub const COMPRESSION_LEVEL_ARG: &str = "compression-level";
const SAMPLE_RATE_ARG: &str = "sample-rate";
const SAMPLE_OFFSET_ARG: &str = "sample-offset";
pub const EXCLUDE_CHECK_TYPE_ARG: &str = "exclude-check-type";
pub const INCLUDE_CHECK_TYPE_ARG: &str = "include-check-type";
pub const SAMPLE_PATH_REGEX_ARG: &str = "sample-path-regex";
const EXCLUDE_SAMPLE_NODE_TYPE_ARG: &str = "exclude-sample-node-type";
const INCLUDE_SAMPLE_NODE_TYPE_ARG: &str = "include-sample-node-type";
pub const EXCLUDE_OUTPUT_NODE_TYPE_ARG: &str = "exclude-output-node-type";
pub const INCLUDE_OUTPUT_NODE_TYPE_ARG: &str = "include-output-node-type";
pub const OUTPUT_FORMAT_ARG: &str = "output-format";
pub const OUTPUT_DIR_ARG: &str = "output-dir";
const SCUBA_TABLE_ARG: &str = "scuba-table";
const SCUBA_LOG_FILE_ARG: &str = "scuba-log-file";

const DEFAULT_VALUE_ARG: &str = "default";
const DERIVED_VALUE_ARG: &str = "derived";
const SHALLOW_VALUE_ARG: &str = "shallow";
const DEEP_VALUE_ARG: &str = "deep";
const MARKER_VALUE_ARG: &str = "marker";
const HG_VALUE_ARG: &str = "hg";
const BONSAI_VALUE_ARG: &str = "bonsai";
const CONTENT_META_VALUE_ARG: &str = "contentmeta";
const ALL_VALUE_ARG: &str = "all";

const DERIVED_PREFIX: &str = "derived_";

static DERIVED_DATA_INCLUDE_NODE_TYPES: Lazy<HashMap<String, Vec<NodeType>>> = Lazy::new(|| {
    let mut m: HashMap<String, Vec<NodeType>> = HashMap::new();
    for t in NodeType::iter() {
        if let Some(n) = t.derived_data_name() {
            m.entry(format!("{}{}", DERIVED_PREFIX, n))
                .or_default()
                .push(t);
        }
    }
    m
});

static NODE_TYPE_POSSIBLE_VALUES: Lazy<Vec<&'static str>> = Lazy::new(|| {
    let mut v = vec![
        ALL_VALUE_ARG,
        BONSAI_VALUE_ARG,
        DEFAULT_VALUE_ARG,
        DERIVED_VALUE_ARG,
        HG_VALUE_ARG,
    ];
    v.extend(
        DERIVED_DATA_INCLUDE_NODE_TYPES
            .keys()
            .map(|e| e.as_ref() as &'static str),
    );
    v.extend(NodeType::VARIANTS.iter());
    v
});

/// Default to clearing out all except HgChangesets ( and bonsai Changsets as no option to clear those)
const DEFAULT_CHUNK_CLEAR_INTERNED_TYPES: &[InternedType] = &[
    InternedType::FileUnodeId,
    InternedType::HgFileNodeId,
    InternedType::HgManifestId,
    InternedType::ManifestUnodeId,
    InternedType::MPathHash,
];

static INTERNED_TYPE_POSSIBLE_VALUES: Lazy<Vec<&'static str>> = Lazy::new(|| {
    let mut v = vec![ALL_VALUE_ARG, DEFAULT_VALUE_ARG];
    v.extend(InternedType::VARIANTS.iter());
    v
});

// We can jump for ChangesetId to all of these:
const CHUNK_BY_PUBLIC_NODE_TYPES: &[NodeType] = &[
    NodeType::BonsaiHgMapping,
    NodeType::PhaseMapping,
    NodeType::Changeset,
    NodeType::ChangesetInfo,
    NodeType::ChangesetInfoMapping,
    NodeType::DeletedManifestMapping,
    NodeType::FsnodeMapping,
    NodeType::SkeletonManifestMapping,
    NodeType::UnodeMapping,
];

static CHUNK_BY_PUBLIC_POSSIBLE_VALUES: Lazy<Vec<&'static str>> = Lazy::new(|| {
    CHUNK_BY_PUBLIC_NODE_TYPES
        .iter()
        .map(|e| e.as_ref() as &'static str)
        .collect()
});

static EDGE_TYPE_POSSIBLE_VALUES: Lazy<Vec<&'static str>> = Lazy::new(|| {
    let mut v = vec![
        DEEP_VALUE_ARG,
        SHALLOW_VALUE_ARG,
        ALL_VALUE_ARG,
        BONSAI_VALUE_ARG,
        HG_VALUE_ARG,
        CONTENT_META_VALUE_ARG,
        MARKER_VALUE_ARG,
    ];
    v.extend(EdgeType::VARIANTS.iter());
    v
});

// Toplevel args - healer and populate healer have this one at top level
// so keeping it there for consistency
const STORAGE_ID_ARG: &str = "storage-id";

const DEFAULT_INCLUDE_NODE_TYPES: &[NodeType] = &[
    NodeType::Bookmark,
    NodeType::Changeset,
    NodeType::BonsaiHgMapping,
    NodeType::PhaseMapping,
    NodeType::PublishedBookmarks,
    NodeType::HgBonsaiMapping,
    NodeType::HgChangeset,
    NodeType::HgChangesetViaBonsai,
    NodeType::HgManifest,
    NodeType::HgFileEnvelope,
    NodeType::HgFileNode,
    NodeType::FileContent,
    NodeType::FileContentMetadata,
    NodeType::AliasContentMapping,
];

const BONSAI_INCLUDE_NODE_TYPES: &[NodeType] = &[NodeType::Bookmark, NodeType::Changeset];

// Goes as far into history as it can
const DEEP_INCLUDE_EDGE_TYPES: &[EdgeType] = &[
    // Bonsai
    EdgeType::BookmarkToChangeset,
    EdgeType::ChangesetToFileContent,
    EdgeType::ChangesetToBonsaiParent,
    EdgeType::ChangesetToBonsaiHgMapping,
    EdgeType::BonsaiHgMappingToHgChangeset,
    EdgeType::PublishedBookmarksToChangeset,
    EdgeType::PublishedBookmarksToBonsaiHgMapping,
    EdgeType::ChangesetToChangesetInfoMapping,
    EdgeType::ChangesetToDeletedManifestMapping,
    EdgeType::ChangesetToFsnodeMapping,
    EdgeType::ChangesetToSkeletonManifestMapping,
    EdgeType::ChangesetToUnodeMapping,
    // Hg
    EdgeType::HgBonsaiMappingToChangeset,
    EdgeType::HgChangesetToHgParent,
    EdgeType::HgChangesetToHgManifest,
    EdgeType::HgChangesetViaBonsaiToHgChangeset,
    EdgeType::HgManifestToHgFileEnvelope,
    EdgeType::HgManifestToHgFileNode,
    EdgeType::HgManifestToChildHgManifest,
    EdgeType::HgFileEnvelopeToFileContent,
    EdgeType::HgFileNodeToLinkedHgBonsaiMapping,
    EdgeType::HgFileNodeToLinkedHgChangeset,
    EdgeType::HgFileNodeToHgParentFileNode,
    EdgeType::HgFileNodeToHgCopyfromFileNode,
    // Content
    EdgeType::FileContentToFileContentMetadata,
    EdgeType::FileContentMetadataToSha1Alias,
    EdgeType::FileContentMetadataToSha256Alias,
    EdgeType::FileContentMetadataToGitSha1Alias,
    EdgeType::AliasContentMappingToFileContent,
    // Derived data
    EdgeType::BlameToChangeset,
    EdgeType::ChangesetInfoMappingToChangesetInfo,
    EdgeType::ChangesetInfoToChangesetInfoParent,
    EdgeType::DeletedManifestMappingToRootDeletedManifest,
    EdgeType::DeletedManifestToDeletedManifestChild,
    EdgeType::DeletedManifestToLinkedChangeset,
    EdgeType::FastlogBatchToChangeset,
    EdgeType::FastlogBatchToPreviousBatch,
    EdgeType::FastlogDirToChangeset,
    EdgeType::FastlogDirToPreviousBatch,
    EdgeType::FastlogFileToChangeset,
    EdgeType::FastlogFileToPreviousBatch,
    EdgeType::FsnodeMappingToRootFsnode,
    EdgeType::FsnodeToChildFsnode,
    EdgeType::FsnodeToFileContent,
    EdgeType::SkeletonManifestMappingToRootSkeletonManifest,
    EdgeType::SkeletonManifestToSkeletonManifestChild,
    EdgeType::UnodeFileToBlame,
    EdgeType::UnodeFileToFastlogFile,
    EdgeType::UnodeFileToFileContent,
    EdgeType::UnodeFileToLinkedChangeset,
    EdgeType::UnodeFileToUnodeFileParent,
    EdgeType::UnodeManifestToFastlogDir,
    EdgeType::UnodeManifestToLinkedChangeset,
    EdgeType::UnodeManifestToUnodeManifestParent,
    EdgeType::UnodeManifestToUnodeFileChild,
    EdgeType::UnodeManifestToUnodeManifestChild,
    EdgeType::UnodeMappingToRootUnodeManifest,
];

// Does not recurse into history, edges to parents excluded
const SHALLOW_INCLUDE_EDGE_TYPES: &[EdgeType] = &[
    // Bonsai
    EdgeType::BookmarkToChangeset,
    EdgeType::ChangesetToFileContent,
    EdgeType::ChangesetToBonsaiHgMapping,
    EdgeType::BonsaiHgMappingToHgChangeset,
    EdgeType::PublishedBookmarksToChangeset,
    EdgeType::PublishedBookmarksToBonsaiHgMapping,
    EdgeType::ChangesetToChangesetInfoMapping,
    EdgeType::ChangesetToDeletedManifestMapping,
    EdgeType::ChangesetToFsnodeMapping,
    EdgeType::ChangesetToSkeletonManifestMapping,
    EdgeType::ChangesetToUnodeMapping,
    // Hg
    EdgeType::HgBonsaiMappingToChangeset,
    EdgeType::HgChangesetToHgManifest,
    EdgeType::HgChangesetViaBonsaiToHgChangeset,
    EdgeType::HgManifestToHgFileEnvelope,
    EdgeType::HgManifestToHgFileNode,
    EdgeType::HgManifestToChildHgManifest,
    EdgeType::HgFileEnvelopeToFileContent,
    // Content
    EdgeType::FileContentToFileContentMetadata,
    EdgeType::FileContentMetadataToSha1Alias,
    EdgeType::FileContentMetadataToSha256Alias,
    EdgeType::FileContentMetadataToGitSha1Alias,
    EdgeType::AliasContentMappingToFileContent,
    // Derived data
    EdgeType::ChangesetInfoMappingToChangesetInfo,
    EdgeType::DeletedManifestMappingToRootDeletedManifest,
    EdgeType::DeletedManifestToDeletedManifestChild,
    EdgeType::FastlogBatchToPreviousBatch,
    EdgeType::FastlogDirToPreviousBatch,
    EdgeType::FastlogFileToPreviousBatch,
    EdgeType::FsnodeToChildFsnode,
    EdgeType::FsnodeToFileContent,
    EdgeType::FsnodeMappingToRootFsnode,
    EdgeType::SkeletonManifestMappingToRootSkeletonManifest,
    EdgeType::SkeletonManifestToSkeletonManifestChild,
    EdgeType::UnodeFileToBlame,
    EdgeType::UnodeFileToFastlogFile,
    EdgeType::UnodeFileToFileContent,
    EdgeType::UnodeManifestToFastlogDir,
    EdgeType::UnodeManifestToUnodeFileChild,
    EdgeType::UnodeManifestToUnodeManifestChild,
    EdgeType::UnodeMappingToRootUnodeManifest,
];

// Types that can result in loading hg data.  Useful for excludes.
const HG_EDGE_TYPES: &[EdgeType] = &[
    // Bonsai to Hg
    EdgeType::BookmarkToBonsaiHgMapping,
    EdgeType::BonsaiHgMappingToHgChangeset,
    EdgeType::PublishedBookmarksToBonsaiHgMapping,
    // Hg
    EdgeType::HgChangesetToHgManifest,
    EdgeType::HgChangesetToHgParent,
    EdgeType::HgChangesetViaBonsaiToHgChangeset,
    EdgeType::HgManifestToHgFileEnvelope,
    EdgeType::HgManifestToHgFileNode,
    EdgeType::HgManifestToChildHgManifest,
    EdgeType::HgFileEnvelopeToFileContent,
    EdgeType::HgFileNodeToLinkedHgChangeset,
    EdgeType::HgFileNodeToHgParentFileNode,
    EdgeType::HgFileNodeToHgCopyfromFileNode,
];

// Types that can result in loading bonsai data
const BONSAI_EDGE_TYPES: &[EdgeType] = &[
    // Bonsai
    EdgeType::BookmarkToChangeset,
    EdgeType::ChangesetToFileContent,
    EdgeType::ChangesetToBonsaiParent,
    EdgeType::PublishedBookmarksToChangeset,
];

const CONTENT_META_EDGE_TYPES: &[EdgeType] = &[
    // Content
    EdgeType::FileContentToFileContentMetadata,
    EdgeType::FileContentMetadataToSha1Alias,
    EdgeType::FileContentMetadataToSha256Alias,
    EdgeType::FileContentMetadataToGitSha1Alias,
    EdgeType::AliasContentMappingToFileContent,
];

#[derive(Clone, Debug, PartialEq, Eq, AsRefStr, EnumVariantNames, EnumString)]
pub enum OutputFormat {
    Debug,
    PrettyDebug,
}

// Things like phases and obs markers will go here
const MARKER_EDGE_TYPES: &[EdgeType] = &[EdgeType::ChangesetToPhaseMapping];

static INCLUDE_NODE_TYPE_HELP: Lazy<String> = Lazy::new(|| {
    format!(
        "Graph node types we want to step to in the walk. Defaults to core Mononoke and Hg types: {:?}. See --{} for all possible values.",
        DEFAULT_INCLUDE_NODE_TYPES, EXCLUDE_NODE_TYPE_ARG
    )
});

static INCLUDE_EDGE_TYPE_HELP: Lazy<String> = Lazy::new(|| {
    format!(
        "Graph edge types to include in the walk. Defaults to deep: {:?}. See --{} for all possible values.",
        DEEP_INCLUDE_EDGE_TYPES, EXCLUDE_EDGE_TYPE_ARG
    )
});

fn add_sampling_args<'a, 'b>(app: App<'a, 'b>) -> App<'a, 'b> {
    app.arg(
        Arg::with_name(LIMIT_DATA_FETCH_ARG)
            .long(LIMIT_DATA_FETCH_ARG)
            .takes_value(false)
            .required(false)
            .help("Limit the amount of data fetched from stores, by not streaming large files to the end."),
    ).arg(
        Arg::with_name(SAMPLE_RATE_ARG)
            .long(SAMPLE_RATE_ARG)
            .takes_value(true)
            .required(false)
            .help("Pass 1 to try all nodes, 120 to do 1 in 120, etc."),
    )
    .arg(
        Arg::with_name(SAMPLE_OFFSET_ARG)
            .long(SAMPLE_OFFSET_ARG)
            .takes_value(true)
            .required(false)
            .help("Offset to apply to the sampling fingerprint for each node, can be used to cycle through an entire repo in N pieces. Default is 0."),
    )
    .arg(
        Arg::with_name(EXCLUDE_SAMPLE_NODE_TYPE_ARG)
            .long(EXCLUDE_SAMPLE_NODE_TYPE_ARG)
            .short("S")
            .takes_value(true)
            .multiple(true)
            .number_of_values(1)
            .required(false)
            .help("Node types to exclude from the sample"),
    )
    .arg(
        Arg::with_name(INCLUDE_SAMPLE_NODE_TYPE_ARG)
            .long(INCLUDE_SAMPLE_NODE_TYPE_ARG)
            .short("s")
            .takes_value(true)
            .multiple(true)
            .number_of_values(1)
            .required(false)
            .help("Node types to include in the sample, defaults to same as the walk."),
    )
    .arg(
        Arg::with_name(SAMPLE_PATH_REGEX_ARG)
            .long(SAMPLE_PATH_REGEX_ARG)
            .takes_value(true)
            .required(false)
            .help("If provided, only sample paths that match"),
    )
}

pub fn parse_sampling_args(
    sub_m: &ArgMatches,
    default_sample_rate: u64,
) -> Result<SamplingOptions, Error> {
    let sample_rate = args::get_u64_opt(&sub_m, SAMPLE_RATE_ARG).unwrap_or(default_sample_rate);
    let sample_offset = args::get_u64_opt(&sub_m, SAMPLE_OFFSET_ARG).unwrap_or(0);
    let node_types = parse_node_types(
        sub_m,
        INCLUDE_SAMPLE_NODE_TYPE_ARG,
        EXCLUDE_SAMPLE_NODE_TYPE_ARG,
        &[],
    )?;
    let exclude_types = parse_node_values(sub_m.values_of(EXCLUDE_SAMPLE_NODE_TYPE_ARG), &[])?;
    Ok(SamplingOptions {
        sample_rate,
        sample_offset,
        node_types,
        exclude_types,
    })
}

pub fn setup_toplevel_app<'a, 'b>(
    app_name: &str,
    cachelib_defaults: CachelibSettings,
) -> MononokeClapApp<'a, 'b> {
    let app_template = args::MononokeAppBuilder::new(app_name)
        .with_blobstore_cachelib_attempt_zstd_default(false)
        .with_blobstore_read_qps_default(NonZeroU32::new(20000))
        .with_readonly_storage_default(ReadOnlyStorage(true))
        .with_repo_required(RepoRequirement::AtLeastOne)
        .with_fb303_args()
        .with_cachelib_settings(cachelib_defaults);

    let scrub_objects =
        setup_subcommand_args(SubCommand::with_name(SCRUB).about("scrub, checks data is present by reading it and counting it. Combine with --enable-scrub-blobstore to check across a multiplex"));
    let scrub_objects = add_sampling_args(scrub_objects)
        .arg(
            Arg::with_name(EXCLUDE_OUTPUT_NODE_TYPE_ARG)
                .long(EXCLUDE_OUTPUT_NODE_TYPE_ARG)
                .short("O")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1)
                .required(false)
                .help("Node types not to output in debug stdout"),
        )
        .arg(
            Arg::with_name(INCLUDE_OUTPUT_NODE_TYPE_ARG)
                .long(INCLUDE_OUTPUT_NODE_TYPE_ARG)
                .short("o")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1)
                .required(false)
                .help("Node types to output in debug stdout"),
        )
        .arg(
            Arg::with_name(OUTPUT_FORMAT_ARG)
                .long(OUTPUT_FORMAT_ARG)
                .short("F")
                .takes_value(true)
                .multiple(false)
                .number_of_values(1)
                .possible_values(OutputFormat::VARIANTS)
                .default_value(OutputFormat::PrettyDebug.as_ref())
                .required(false)
                .help("Set the output format"),
        );

    let compression_benefit = setup_subcommand_args(
        SubCommand::with_name(COMPRESSION_BENEFIT).about("estimate compression benefit"),
    )
    .arg(
        Arg::with_name(COMPRESSION_LEVEL_ARG)
            .long(COMPRESSION_LEVEL_ARG)
            .takes_value(true)
            .required(false)
            .help("Zstd compression level to use. 3 is the default"),
    );
    let compression_benefit = add_sampling_args(compression_benefit);

    let corpus = setup_subcommand_args(
        SubCommand::with_name(CORPUS).about("Dump a sampled corpus of blobstore data"),
    )
    .arg(
        Arg::with_name(OUTPUT_DIR_ARG)
            .long(OUTPUT_DIR_ARG)
            .takes_value(true)
            .required(false)
            .help("Where to write the output corpus. Default is to to a dry run with no output."),
    );
    let corpus = add_sampling_args(corpus);

    let validate = setup_subcommand_args(
        SubCommand::with_name(VALIDATE).about("walk the graph and perform checks on it"),
    )
    .arg(
        Arg::with_name(EXCLUDE_CHECK_TYPE_ARG)
            .long(EXCLUDE_CHECK_TYPE_ARG)
            .short("C")
            .takes_value(true)
            .multiple(true)
            .number_of_values(1)
            .required(false)
            .help("Checks to exclude"),
    )
    .arg(
        Arg::with_name(INCLUDE_CHECK_TYPE_ARG)
            .long(INCLUDE_CHECK_TYPE_ARG)
            .short("c")
            .takes_value(true)
            .multiple(true)
            .number_of_values(1)
            .required(false)
            .possible_values(CheckType::VARIANTS)
            .help("Check types to include, defaults to all possible values"),
    );

    app_template.build()
        .about("Walks the mononoke commit and/or derived data graphs, with option of performing validations and modifications")
        .arg(
            Arg::with_name(STORAGE_ID_ARG)
                .long(STORAGE_ID_ARG)
                .takes_value(true)
                .required(false)
                .help("id of storage group to operate over, e.g. manifold_xdb_multiplex"),
        )
        .subcommand(compression_benefit)
        .subcommand(corpus)
        .subcommand(scrub_objects)
        .subcommand(validate)
}

// Add the args the "start from repo" walk types need
fn setup_subcommand_args<'a, 'b>(subcmd: App<'a, 'b>) -> App<'a, 'b> {
    return subcmd
        .arg(
            Arg::with_name(QUIET_ARG)
                .long(QUIET_ARG)
                .short("q")
                .takes_value(false)
                .required(false)
                .help("Log a lot less"),
        )
        .arg(
            Arg::with_name(ENABLE_REDACTION_ARG)
                .long(ENABLE_REDACTION_ARG)
                .takes_value(false)
                .required(false)
                .help("Use redaction from config. Default is redaction off."),
        )
        .arg(
            Arg::with_name(SCHEDULED_MAX_ARG)
                .long(SCHEDULED_MAX_ARG)
                .takes_value(true)
                .required(false)
                .help("Maximum number of walk step tasks to attempt to execute at once.  Default 4096."),
        )
        .arg(
            Arg::with_name(TAIL_INTERVAL_ARG)
                .long(TAIL_INTERVAL_ARG)
                .takes_value(true)
                .required(false)
                .help("Tail by polling the entry points at interval of TAIL seconds"),
        )
        .arg(
            Arg::with_name(PROGRESS_INTERVAL_ARG)
                .long(PROGRESS_INTERVAL_ARG)
                .takes_value(true)
                .required(false)
                .help("Minimum interval between progress reports in seconds."),
        )
        .arg(
            Arg::with_name(PROGRESS_SAMPLE_RATE_ARG)
                .long(PROGRESS_SAMPLE_RATE_ARG)
                .takes_value(true)
                .required(false)
                .help("Sample the walk output stream for progress roughly 1 in N steps. Only log if progress-interval has passed."),
        )
        .arg(
            Arg::with_name(ENABLE_DERIVE_ARG)
                .long(ENABLE_DERIVE_ARG)
                .takes_value(false)
                .required(false)
                .help("Enable derivation of data (e.g. hg, file metadata). Default is false"),
        )
        .arg(
            Arg::with_name(SCRUB_BLOBSTORE_ACTION_ARG)
                .long(SCRUB_BLOBSTORE_ACTION_ARG)
                .takes_value(true)
                .required(false)
                .help("Enable ScrubBlobstore with the given action. Checks for keys missing from stores. In ReportOnly mode this logs only, otherwise it performs a copy to the missing stores."),
        )
        .arg(
            Arg::with_name(EXCLUDE_NODE_TYPE_ARG)
                .long(EXCLUDE_NODE_TYPE_ARG)
                .short("x")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1)
                .required(false)
                .possible_values(&NODE_TYPE_POSSIBLE_VALUES)
                .help("Graph node types to exclude from walk. They are removed from the include node types."),
        )
        .arg(
            Arg::with_name(INCLUDE_NODE_TYPE_ARG)
                .long(INCLUDE_NODE_TYPE_ARG)
                .short("i")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1)
                .required(false)
                .default_value(DEFAULT_VALUE_ARG)
                .possible_values(&NODE_TYPE_POSSIBLE_VALUES)
                .hide_possible_values(true)
                .help(&INCLUDE_NODE_TYPE_HELP),
        )
        .arg(
            Arg::with_name(EXCLUDE_EDGE_TYPE_ARG)
                .long(EXCLUDE_EDGE_TYPE_ARG)
                .short("X")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1)
                .required(false)
                .possible_values(&EDGE_TYPE_POSSIBLE_VALUES)
                .help("Graph edge types to exclude from walk. Can pass pre-configured sets via deep, shallow, hg, bonsai, etc as well as individual types."),
        )
        .arg(
            Arg::with_name(INCLUDE_EDGE_TYPE_ARG)
                .long(INCLUDE_EDGE_TYPE_ARG)
                .short("I")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1)
                .required(false)
                .default_value(DEEP_VALUE_ARG)
                .possible_values(&EDGE_TYPE_POSSIBLE_VALUES)
                .hide_possible_values(true)
                .help(&INCLUDE_EDGE_TYPE_HELP),
        )
        .arg(
            Arg::with_name(BOOKMARK_ARG)
                .long(BOOKMARK_ARG)
                .short("b")
                .takes_value(true)
                .required(false)
                .multiple(true)
                .number_of_values(1)
                .help("Bookmark(s) to start traversal from"),
        )
        .arg(
            Arg::with_name(WALK_ROOT_ARG)
                .long(WALK_ROOT_ARG)
                .short("r")
                .takes_value(true)
                .required(false)
                .multiple(true)
                .number_of_values(1)
                .help("Root(s) to start traversal from in format <NodeType>:<node_key>, e.g. Bookmark:master or HgChangeset:7712b62acdc858689504945ac8965a303ded6626"),
        )
        .arg(
            Arg::with_name(CHUNK_BY_PUBLIC_ARG)
                .long(CHUNK_BY_PUBLIC_ARG)
                // p because its chunk from public changeset id (c and C already taken)
                .short("p")
                .takes_value(true)
                .required(false)
                .multiple(false)
                .number_of_values(1)
                .possible_values(&CHUNK_BY_PUBLIC_POSSIBLE_VALUES)
                .help("Traverse using chunks of public changesets as roots to the specified node type"),
        )
        .arg(
            Arg::with_name(CHUNK_SIZE_ARG)
                .long(CHUNK_SIZE_ARG)
                .short("k")
                .default_value("100000")
                .takes_value(true)
                .required(false)
                .multiple(false)
                .number_of_values(1)
                .help("How many changesets to include in a chunk."),
        )
        .arg(
            Arg::with_name(CHUNK_CLEAR_SAMPLE_RATE_ARG)
                .long(CHUNK_CLEAR_SAMPLE_RATE_ARG)
                .short("K")
                .takes_value(true)
                .required(false)
                .help("Clear the saved walk state 1 in N steps."),
        )
        .arg(
            Arg::with_name(INCLUDE_CHUNK_CLEAR_INTERNED_TYPE_ARG)
                .long(INCLUDE_CHUNK_CLEAR_INTERNED_TYPE_ARG)
                .short("t")
                .takes_value(true)
                .required(false)
                .multiple(false)
                .number_of_values(1)
                .possible_values(&INTERNED_TYPE_POSSIBLE_VALUES)
                .help("Include in InternedTypes to flush between chunks"),
        )
        .arg(
            Arg::with_name(EXCLUDE_CHUNK_CLEAR_INTERNED_TYPE_ARG)
                .long(EXCLUDE_CHUNK_CLEAR_INTERNED_TYPE_ARG)
                .short("T")
                .takes_value(true)
                .required(false)
                .multiple(false)
                .number_of_values(1)
                .possible_values(&INTERNED_TYPE_POSSIBLE_VALUES)
                .help("Exclude from InternedTypes to flush between chunks"),
        )
        .arg(
            Arg::with_name(INCLUDE_CHUNK_CLEAR_NODE_TYPE_ARG)
                .long(INCLUDE_CHUNK_CLEAR_NODE_TYPE_ARG)
                .short("n")
                .takes_value(true)
                .required(false)
                .multiple(false)
                .number_of_values(1)
                .possible_values(&NODE_TYPE_POSSIBLE_VALUES)
                .help("Include in NodeTypes to flush between chunks"),
        )
        .arg(
            Arg::with_name(EXCLUDE_CHUNK_CLEAR_NODE_TYPE_ARG)
                .long(EXCLUDE_CHUNK_CLEAR_NODE_TYPE_ARG)
                .short("N")
                .takes_value(true)
                .required(false)
                .multiple(false)
                .number_of_values(1)
                .possible_values(&NODE_TYPE_POSSIBLE_VALUES)
                .help("Exclude from NodeTypes to flush between chunks"),
        )
        .arg(
            Arg::with_name(CHECKPOINT_NAME_ARG)
                .long(CHECKPOINT_NAME_ARG)
                .takes_value(true)
                .required(false)
                .help("Name of checkpoint."),
        )
        .arg(
            Arg::with_name(CHECKPOINT_PATH_ARG)
                .long(CHECKPOINT_PATH_ARG)
                .takes_value(true)
                .required(false)
                .requires(CHECKPOINT_NAME_ARG)
                .help("Path for sqlite checkpoint db if using sqlite"),
        )
        .arg(
            Arg::with_name(CHECKPOINT_SAMPLE_RATE_ARG)
                .long(CHECKPOINT_SAMPLE_RATE_ARG)
                .takes_value(true)
                .required(false)
                .default_value("1")
                .help("Checkpoint the walk covered bounds 1 in N steps."),
        )
        .arg(
            Arg::with_name(STATE_MAX_AGE_ARG)
                .long(STATE_MAX_AGE_ARG)
                .takes_value(true)
                .required(false)
                // 5 days = 5 * 24 * 3600 seconds = 432000
                .default_value("432000")
                .help("Max age of walk state held internally ot loaded from checkpoint that we will attempt to continue from, in seconds."),
        )
        .arg(
            Arg::with_name(ALLOW_REMAINING_DEFERRED_ARG)
                .long(ALLOW_REMAINING_DEFERRED_ARG)
                .takes_value(true)
                .required(false)
                .default_value("false")
                .help("Whether to allow remaining deferred edges after chunks complete. Well structured repos should have none."),
        )
        .arg(
            Arg::with_name(ERROR_AS_DATA_NODE_TYPE_ARG)
                .long(ERROR_AS_DATA_NODE_TYPE_ARG)
                .short("e")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1)
                .required(false)
                .help("Use this to continue walking even walker found an error.  Types of nodes to allow the walker to convert an ErrorKind::NotTraversable to a NodeData::ErrorAsData(NotTraversable)"),
        )
        .arg(
            Arg::with_name(ERROR_AS_DATA_EDGE_TYPE_ARG)
                .long(ERROR_AS_DATA_EDGE_TYPE_ARG)
                .short("E")
                .takes_value(true)
                .multiple(true)
                .number_of_values(1)
                .required(false)
                .help("Types of edges to allow the walker to convert an ErrorKind::NotTraversable to a NodeData::ErrorAsData(NotTraversable). If empty then allow all edges for the nodes specified via error-as-data-node-type"),
        )
        .arg(
            Arg::with_name(INNER_BLOBSTORE_ID_ARG)
                .long(INNER_BLOBSTORE_ID_ARG)
                .takes_value(true)
                .required(false)
                .help("If main blobstore in the storage config is a multiplexed one, use inner blobstore with this id")
        )
        .arg(
            Arg::with_name(SCUBA_TABLE_ARG)
                .long(SCUBA_TABLE_ARG)
                .takes_value(true)
                .multiple(false)
                .required(false)
                .help("Scuba table for logging nodes with issues. e.g. mononoke_walker"),
        )
        .arg(
            Arg::with_name(SCUBA_LOG_FILE_ARG)
                .long(SCUBA_LOG_FILE_ARG)
                .takes_value(true)
                .multiple(false)
                .required(false)
                .help("A log file to write Scuba logs to (primarily useful in testing)"),
        );
}

pub fn parse_progress_args(sub_m: &ArgMatches) -> ProgressOptions {
    let sample_rate =
        args::get_u64_opt(&sub_m, PROGRESS_SAMPLE_RATE_ARG).unwrap_or(PROGRESS_SAMPLE_RATE);
    let interval_secs =
        args::get_u64_opt(&sub_m, PROGRESS_INTERVAL_ARG).unwrap_or(PROGRESS_SAMPLE_DURATION_S);

    ProgressOptions {
        sample_rate,
        interval: Duration::from_secs(interval_secs),
    }
}

// parse the pre-defined groups we have for default etc
fn parse_interned_value(
    arg: &str,
    default: &[InternedType],
) -> Result<HashSet<InternedType>, Error> {
    Ok(match arg {
        ALL_VALUE_ARG => InternedType::iter().collect(),
        DEFAULT_VALUE_ARG => default.iter().cloned().collect(),
        _ => InternedType::from_str(arg)
            .map(|e| hashset![e])
            .with_context(|| format_err!("Unknown InternedType {}", arg))?,
    })
}

fn parse_interned_values(
    values: Option<Values>,
    default: &[InternedType],
) -> Result<HashSet<InternedType>, Error> {
    match values {
        None => Ok(default.iter().cloned().collect()),
        Some(values) => process_results(values.map(|v| parse_interned_value(v, default)), |s| {
            s.concat()
        }),
    }
}

pub fn parse_interned_types(
    sub_m: &ArgMatches,
    include_arg_name: &str,
    exclude_arg_name: &str,
    default: &[InternedType],
) -> Result<HashSet<InternedType>, Error> {
    let mut include_types = parse_interned_values(sub_m.values_of(include_arg_name), default)?;
    let exclude_types = parse_interned_values(sub_m.values_of(exclude_arg_name), &[])?;
    include_types.retain(|x| !exclude_types.contains(x));
    Ok(include_types)
}

async fn parse_tail_args<'a>(
    fb: FacebookInit,
    sub_m: &'a ArgMatches<'a>,
    dbconfig: &'a MetadataDatabaseConfig,
    mysql_options: &'a MysqlOptions,
) -> Result<TailParams, Error> {
    let tail_secs = args::get_u64_opt(&sub_m, TAIL_INTERVAL_ARG);

    let public_changeset_chunk_by = parse_node_values(sub_m.values_of(CHUNK_BY_PUBLIC_ARG), &[])?;
    let public_changeset_chunk_size = if !public_changeset_chunk_by.is_empty() {
        args::get_usize_opt(sub_m, CHUNK_SIZE_ARG)
    } else {
        None
    };

    let clear_interned_types = parse_interned_types(
        sub_m,
        INCLUDE_CHUNK_CLEAR_INTERNED_TYPE_ARG,
        EXCLUDE_CHUNK_CLEAR_INTERNED_TYPE_ARG,
        DEFAULT_CHUNK_CLEAR_INTERNED_TYPES,
    )?;

    let clear_node_types = parse_node_types(
        sub_m,
        INCLUDE_CHUNK_CLEAR_NODE_TYPE_ARG,
        EXCLUDE_CHUNK_CLEAR_NODE_TYPE_ARG,
        &[],
    )?;

    let clear_sample_rate = args::get_u64_opt(&sub_m, CHUNK_CLEAR_SAMPLE_RATE_ARG);

    let checkpoint_name = sub_m.value_of(CHECKPOINT_NAME_ARG).map(|s| s.to_string());

    let checkpoints = if let Some(checkpoint_name) = checkpoint_name {
        let checkpoint_path = sub_m.value_of(CHECKPOINT_PATH_ARG).map(|s| s.to_string());
        let sql_checkpoints = if let Some(checkpoint_path) = checkpoint_path {
            SqlCheckpoints::with_sqlite_path(checkpoint_path, false)?
        } else {
            SqlCheckpoints::with_metadata_database_config(fb, dbconfig, mysql_options, false)
                .await?
        };

        Some(CheckpointsByName::new(checkpoint_name, sql_checkpoints))
    } else {
        None
    };

    // Can unwrap these as they have a clap default set
    let state_max_age = args::get_u64_opt(&sub_m, STATE_MAX_AGE_ARG)
        .map(Duration::from_secs)
        .unwrap();
    let checkpoint_sample_rate = args::get_u64_opt(&sub_m, CHECKPOINT_SAMPLE_RATE_ARG).unwrap();
    let allow_remaining_deferred =
        args::get_bool_opt(&sub_m, ALLOW_REMAINING_DEFERRED_ARG).unwrap();

    Ok(TailParams {
        tail_secs,
        public_changeset_chunk_size,
        public_changeset_chunk_by,
        clear_interned_types,
        clear_node_types,
        clear_sample_rate,
        checkpoints,
        state_max_age,
        checkpoint_sample_rate,
        allow_remaining_deferred,
    })
}

// parse the pre-defined groups we have for default etc
fn parse_node_value(arg: &str) -> Result<HashSet<NodeType>, Error> {
    Ok(match arg {
        ALL_VALUE_ARG => HashSet::from_iter(NodeType::iter()),
        DEFAULT_VALUE_ARG => HashSet::from_iter(DEFAULT_INCLUDE_NODE_TYPES.iter().cloned()),
        BONSAI_VALUE_ARG => HashSet::from_iter(BONSAI_INCLUDE_NODE_TYPES.iter().cloned()),
        DERIVED_VALUE_ARG => {
            HashSet::from_iter(DERIVED_DATA_INCLUDE_NODE_TYPES.values().flatten().cloned())
        }
        HG_VALUE_ARG => {
            let mut s = HashSet::new();
            for d in &[MappedHgChangesetId::NAME, FilenodesOnlyPublic::NAME] {
                let d = DERIVED_DATA_INCLUDE_NODE_TYPES.get(&format!("{}{}", DERIVED_PREFIX, d));
                s.extend(d.unwrap().iter().cloned());
            }
            s
        }
        _ => {
            if let Some(v) = DERIVED_DATA_INCLUDE_NODE_TYPES.get(arg) {
                HashSet::from_iter(v.iter().cloned())
            } else {
                NodeType::from_str(arg)
                    .map(|e| hashset![e])
                    .with_context(|| format_err!("Unknown NodeType {}", arg))?
            }
        }
    })
}

fn parse_node_values(
    values: Option<Values>,
    default: &[NodeType],
) -> Result<HashSet<NodeType>, Error> {
    match values {
        None => Ok(HashSet::from_iter(default.iter().cloned())),
        Some(values) => process_results(values.map(parse_node_value), |s| s.concat()),
    }
}

pub fn parse_node_types<'a>(
    sub_m: &impl Borrow<ArgMatches<'a>>,
    include_arg_name: &str,
    exclude_arg_name: &str,
    default: &[NodeType],
) -> Result<HashSet<NodeType>, Error> {
    let sub_m = sub_m.borrow();
    let mut include_node_types = parse_node_values(sub_m.values_of(include_arg_name), default)?;
    let exclude_node_types = parse_node_values(sub_m.values_of(exclude_arg_name), &[])?;
    include_node_types.retain(|x| !exclude_node_types.contains(x));
    Ok(include_node_types)
}

// parse the pre-defined groups we have for deep, shallow, hg, bonsai etc.
fn parse_edge_value(arg: &str) -> Result<HashSet<EdgeType>, Error> {
    Ok(match arg {
        ALL_VALUE_ARG => HashSet::from_iter(EdgeType::iter()),
        BONSAI_VALUE_ARG => HashSet::from_iter(BONSAI_EDGE_TYPES.iter().cloned()),
        CONTENT_META_VALUE_ARG => HashSet::from_iter(CONTENT_META_EDGE_TYPES.iter().cloned()),
        DEEP_VALUE_ARG => HashSet::from_iter(DEEP_INCLUDE_EDGE_TYPES.iter().cloned()),
        MARKER_VALUE_ARG => HashSet::from_iter(MARKER_EDGE_TYPES.iter().cloned()),
        HG_VALUE_ARG => HashSet::from_iter(HG_EDGE_TYPES.iter().cloned()),
        SHALLOW_VALUE_ARG => HashSet::from_iter(SHALLOW_INCLUDE_EDGE_TYPES.iter().cloned()),
        _ => EdgeType::from_str(arg)
            .map(|e| hashset![e])
            .with_context(|| format_err!("Unknown EdgeType {}", arg))?,
    })
}

fn parse_edge_values(
    values: Option<Values>,
    default: &[EdgeType],
) -> Result<HashSet<EdgeType>, Error> {
    match values {
        None => Ok(HashSet::from_iter(default.iter().cloned())),
        Some(values) => process_results(values.map(parse_edge_value), |s| s.concat()),
    }
}

fn parse_edge_types<'a>(
    sub_m: &impl Borrow<ArgMatches<'a>>,
    include_arg_name: &str,
    exclude_arg_name: &str,
    default: &[EdgeType],
) -> Result<HashSet<EdgeType>, Error> {
    let sub_m = sub_m.borrow();
    let mut include_edge_types = parse_edge_values(sub_m.values_of(include_arg_name), default)?;
    let exclude_edge_types = parse_edge_values(sub_m.values_of(exclude_arg_name), &[])?;
    include_edge_types.retain(|x| !exclude_edge_types.contains(x));
    Ok(include_edge_types)
}

fn reachable_graph_elements(
    mut include_edge_types: HashSet<EdgeType>,
    mut include_node_types: HashSet<NodeType>,
    root_node_types: &HashSet<NodeType>,
) -> (HashSet<EdgeType>, HashSet<NodeType>) {
    // This stops us logging that we're walking unreachable edge/node types
    let mut param_count = &include_edge_types.len() + &include_node_types.len();
    let mut last_param_count = 0;
    while param_count != last_param_count {
        let include_edge_types_stable = include_edge_types.clone();
        // Only retain edge types that are traversable
        include_edge_types.retain(|e| {
            e.incoming_type()
                .map(|t|
                    // its an incoming_type we want
                    (include_node_types.contains(&t) || root_node_types.contains(&t)) &&
                    // Another existing edge can get us to this node type
                    (root_node_types.contains(&t) || include_edge_types_stable.iter().any(|o| &o.outgoing_type() == &t)))
                .unwrap_or(true)
                // its an outgoing_type we want
                && include_node_types.contains(&e.outgoing_type())
        });
        // Only retain node types we expect to step to after graph entry
        include_node_types.retain(|t| {
            include_edge_types.iter().any(|e| {
                &e.outgoing_type() == t || e.incoming_type().map(|ot| &ot == t).unwrap_or(false)
            })
        });
        last_param_count = param_count;
        param_count = &include_edge_types.len() + &include_node_types.len();
    }
    (include_edge_types, include_node_types)
}

pub async fn setup_common<'a>(
    walk_stats_key: &'static str,
    fb: FacebookInit,
    logger: &'a Logger,
    blobstore_sampler: Option<Arc<dyn SamplingHandler>>,
    matches: &'a MononokeMatches<'a>,
    sub_m: &'a ArgMatches<'a>,
) -> Result<(JobWalkParams, Vec<(RepoSubcommandParams, RepoWalkParams)>), Error> {
    let config_store = args::init_config_store(fb, logger, matches)?;

    let quiet = sub_m.is_present(QUIET_ARG);
    let common_config = cmdlib::args::load_common_config(config_store, &matches)?;
    let scheduled_max = args::get_usize_opt(&sub_m, SCHEDULED_MAX_ARG).unwrap_or(4096) as usize;
    let inner_blobstore_id = args::get_u64_opt(&sub_m, INNER_BLOBSTORE_ID_ARG);
    let progress_options = parse_progress_args(sub_m);

    let enable_derive = sub_m.is_present(ENABLE_DERIVE_ARG);

    let caching = matches.parse_and_init_cachelib(fb);

    let include_edge_types = parse_edge_types(
        sub_m,
        INCLUDE_EDGE_TYPE_ARG,
        EXCLUDE_EDGE_TYPE_ARG,
        DEEP_INCLUDE_EDGE_TYPES,
    )?;

    let include_node_types = parse_node_types(
        sub_m,
        INCLUDE_NODE_TYPE_ARG,
        EXCLUDE_NODE_TYPE_ARG,
        DEFAULT_INCLUDE_NODE_TYPES,
    )?;

    let mut walk_roots: Vec<OutgoingEdge> = vec![];

    if sub_m.is_present(BOOKMARK_ARG) {
        let bookmarks: Result<Vec<BookmarkName>, Error> = match sub_m.values_of(BOOKMARK_ARG) {
            None => Err(format_err!("No bookmark passed to --{}", BOOKMARK_ARG)),
            Some(values) => values.map(BookmarkName::new).collect(),
        };

        let mut bookmarks = bookmarks?
            .into_iter()
            .map(|b| OutgoingEdge::new(EdgeType::RootToBookmark, Node::Bookmark(b)))
            .collect();
        walk_roots.append(&mut bookmarks);
    }

    if sub_m.is_present(WALK_ROOT_ARG) {
        let roots: Vec<_> = match sub_m.values_of(WALK_ROOT_ARG) {
            None => Err(format_err!("No root node passed to --{}", WALK_ROOT_ARG)),
            Some(values) => values.map(|root| parse_node(root)).collect(),
        }?;
        let mut roots = roots
            .into_iter()
            .filter_map(|node| {
                node.get_type()
                    .root_edge_type()
                    .map(|et| OutgoingEdge::new(et, node))
            })
            .collect();
        walk_roots.append(&mut roots);
    }

    let readonly_storage = args::parse_readonly_storage(&matches);

    let error_as_data_node_types = parse_node_types(
        sub_m,
        ERROR_AS_DATA_NODE_TYPE_ARG,
        EXCLUDE_NODE_TYPE_ARG,
        &[],
    )?;
    let error_as_data_edge_types = parse_edge_types(
        sub_m,
        ERROR_AS_DATA_EDGE_TYPE_ARG,
        EXCLUDE_EDGE_TYPE_ARG,
        &[],
    )?;
    if !error_as_data_node_types.is_empty() || !error_as_data_edge_types.is_empty() {
        if !readonly_storage.0 {
            return Err(format_err!(
                "Error as data could mean internal state is invalid, run with --with-readonly-storage=true to ensure no risk of persisting it"
            ));
        }
        warn!(
            logger,
            "Error as data enabled, walk results may not be complete. Errors as data enabled for node types {:?} edge types {:?}",
            sort_by_string(&error_as_data_node_types),
            sort_by_string(&error_as_data_edge_types)
        );
    }

    let mysql_options = args::parse_mysql_options(&matches);
    let mut blobstore_options = args::parse_blobstore_options(&matches)?;
    let storage_id = matches.value_of(STORAGE_ID_ARG);
    let scrub_action = sub_m
        .value_of(SCRUB_BLOBSTORE_ACTION_ARG)
        .map(ScrubAction::from_str)
        .transpose()?;

    let enable_redaction = sub_m.is_present(ENABLE_REDACTION_ARG);

    // Setup scuba
    let scuba_table = sub_m.value_of(SCUBA_TABLE_ARG).map(|a| a.to_string());
    let mut scuba_builder = MononokeScubaSampleBuilder::with_opt_table(fb, scuba_table);
    scuba_builder.add_common_server_data();
    scuba_builder.add(WALK_TYPE, walk_stats_key);
    if let Some(scuba_log_file) = sub_m.value_of(SCUBA_LOG_FILE_ARG) {
        scuba_builder = scuba_builder.with_log_file(scuba_log_file)?;
    }

    // Resolve repo ids and names
    let repos = args::resolve_repos(config_store, &matches)?;
    let repo_count = repos.len();
    if repo_count > 1 {
        info!(
            logger,
            "Walking repos {:?}",
            repos.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
    }
    let mut repo_id_to_name = HashMap::new();
    for repo in &repos {
        repo_id_to_name.insert(repo.id, repo.name.clone());
    }

    if let Some(scrub_action) = scrub_action {
        blobstore_options.scrub_options = Some(ScrubOptions {
            scrub_action,
            scrub_handler: Arc::new(blobstore::StatsScrubHandler::new(
                false,
                scuba_builder.clone(),
                walk_stats_key,
                repo_id_to_name.clone(),
            )) as Arc<dyn ScrubHandler>,
        });
    }

    let storage_override = if let Some(storage_id) = storage_id {
        let mut configs = args::load_storage_configs(config_store, &matches)?;
        let storage_config = configs.storage.remove(storage_id).ok_or_else(|| {
            format_err!(
                "Storage id `{}` not found in {:?}",
                storage_id,
                configs.storage.keys()
            )
        })?;
        Some(storage_config)
    } else {
        None
    };

    let mut metadatadb_config_to_blob_config =
        HashMap::<MetadataDatabaseConfig, HashSet<BlobConfig>>::new();
    let mut blob_config_to_repos = HashMap::<BlobConfig, Vec<ResolvedRepo>>::new();
    for repo in repos {
        let storage_config = storage_override
            .clone()
            .unwrap_or_else(|| repo.config.storage_config.clone());
        metadatadb_config_to_blob_config
            .entry(storage_config.metadata)
            .or_default()
            .insert(storage_config.blobstore.clone());
        blob_config_to_repos
            .entry(storage_config.blobstore)
            .or_default()
            .push(repo);
    }

    let mut per_repo = Vec::new();

    // First setup SQL config
    for (metadatadb_config, blobconfigs) in metadatadb_config_to_blob_config {
        let tail_params = parse_tail_args(fb, sub_m, &metadatadb_config, &mysql_options).await?;

        if tail_params.public_changeset_chunk_by.is_empty() && walk_roots.is_empty() {
            bail!(
                "No walk roots provided, pass with  --{}, --{} or --{}",
                BOOKMARK_ARG,
                WALK_ROOT_ARG,
                CHUNK_BY_PUBLIC_ARG,
            );
        }

        let sql_factory = make_metadata_sql_factory(
            fb,
            metadatadb_config,
            mysql_options.clone(),
            readonly_storage,
            logger.clone(),
        )
        .compat()
        .await?;

        // Share the sql factory with the blobstores associated with it
        for blobconfig in blobconfigs {
            let repos = blob_config_to_repos.get(&blobconfig);

            // Open the blobstore explicitly so we can do things like run on one side of a multiplex
            let blobstore = Arc::new(
                blobstore::open_blobstore(
                    fb,
                    &mysql_options,
                    blobconfig,
                    inner_blobstore_id,
                    readonly_storage,
                    blobstore_sampler.clone(),
                    walk_stats_key,
                    repo_id_to_name.clone(),
                    &blobstore_options,
                    &logger,
                    config_store,
                )
                .await?,
            );

            // Build the per-repo structures sharing common blobstore
            for repo in repos.into_iter().flatten() {
                let one_repo = setup_repo(
                    walk_stats_key,
                    fb,
                    logger,
                    scuba_builder.clone(),
                    blobstore.clone(),
                    &sql_factory,
                    readonly_storage,
                    caching,
                    blobstore_options.cachelib_options.clone(),
                    enable_redaction,
                    common_config.censored_scuba_params.clone(),
                    scheduled_max,
                    repo_count,
                    repo,
                    walk_roots.clone(),
                    tail_params.clone(),
                    include_edge_types.clone(),
                    include_node_types.clone(),
                    progress_options,
                )
                .await?;
                per_repo.push(one_repo);
            }
        }
    }

    Ok((
        JobWalkParams {
            enable_derive,
            quiet,
            error_as_data_node_types,
            error_as_data_edge_types,
            repo_count,
        },
        per_repo,
    ))
}

// Setup for just one repo. Try and keep clap parsing out of here, should be done beforehand
async fn setup_repo<'a>(
    walk_stats_key: &'static str,
    fb: FacebookInit,
    logger: &'a Logger,
    mut scuba_builder: MononokeScubaSampleBuilder,
    blobstore: Arc<dyn Blobstore>,
    sql_factory: &'a MetadataSqlFactory,
    readonly_storage: ReadOnlyStorage,
    caching: Caching,
    cachelib_blobstore_options: CachelibBlobstoreOptions,
    enable_redaction: bool,
    redaction_scuba_params: CensoredScubaParams,
    scheduled_max: usize,
    repo_count: usize,
    resolved: &'a ResolvedRepo,
    walk_roots: Vec<OutgoingEdge>,
    mut tail_params: TailParams,
    include_edge_types: HashSet<EdgeType>,
    mut include_node_types: HashSet<NodeType>,
    progress_options: ProgressOptions,
) -> Result<(RepoSubcommandParams, RepoWalkParams), Error> {
    let logger = if repo_count > 1 {
        logger.new(o!("repo" => resolved.name.clone()))
    } else {
        logger.clone()
    };

    let scheduled_max = scheduled_max / repo_count;
    scuba_builder.add(REPO, resolved.name.clone());

    // Only walk derived node types that the repo is configured to contain
    include_node_types.retain(|t| {
        if let Some(t) = t.derived_data_name() {
            resolved.config.derived_data_config.is_enabled(t)
        } else {
            true
        }
    });

    tail_params.public_changeset_chunk_by.retain(|t| {
        if let Some(t) = t.derived_data_name() {
            resolved.config.derived_data_config.is_enabled(t)
        } else {
            true
        }
    });

    let mut root_node_types: HashSet<_> =
        walk_roots.iter().map(|e| e.label.outgoing_type()).collect();
    if tail_params.public_changeset_chunk_size.is_some() {
        root_node_types.extend(tail_params.public_changeset_chunk_by.iter().cloned());
    }

    let (include_edge_types, include_node_types) =
        reachable_graph_elements(include_edge_types, include_node_types, &root_node_types);
    info!(
        logger,
        #log::GRAPH,
        "Walking edge types {:?}",
        sort_by_string(&include_edge_types)
    );
    info!(
        logger,
        #log::GRAPH,
        "Walking node types {:?}",
        sort_by_string(&include_node_types)
    );

    let redaction = if enable_redaction {
        resolved.config.redaction
    } else {
        Redaction::Disabled
    };

    let repo = open_blobrepo_given_datasources(
        fb,
        blobstore,
        &sql_factory,
        &resolved.config,
        caching,
        redaction,
        redaction_scuba_params,
        readonly_storage,
        resolved.name.clone(),
        cachelib_blobstore_options,
    );

    scuba_builder.add(REPO, resolved.name.clone());

    let mut progress_node_types = include_node_types.clone();
    for e in &walk_roots {
        progress_node_types.insert(e.target.get_type());
    }

    let progress_state = ProgressStateMutex::new(ProgressStateCountByType::new(
        fb,
        logger.clone(),
        walk_stats_key,
        resolved.name.clone(),
        progress_node_types,
        progress_options,
    ));

    Ok((
        RepoSubcommandParams {
            progress_state,
            tail_params,
        },
        RepoWalkParams {
            repo: repo.await?,
            logger: logger.clone(),
            scheduled_max,
            walk_roots,
            include_node_types,
            include_edge_types,
            scuba_builder,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_parse_node_value() {
        let r = parse_node_value("bad_node_type");
        assert!(r.is_err());
    }

    #[test]
    fn bad_parse_node_values() {
        let m = App::new("test")
            .arg(
                Arg::with_name(INCLUDE_NODE_TYPE_ARG)
                    .short("i")
                    .multiple(true)
                    .takes_value(true),
            )
            .get_matches_from(vec!["test", "-i", "bad_node_type"]);
        let r = parse_node_values(m.values_of(INCLUDE_NODE_TYPE_ARG), &[]);
        assert!(r.is_err());
    }
}
