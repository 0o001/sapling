/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

// This module allows the implementation of validating checks over the mononoke graph
// Currently checks are added by
//  1. Add a CheckType variant
//  2. Add CheckType::node_type() and CheckType::enum_type() cases for the new variant
//  3. Add a new validation method
//  4. Add the method to the match/case in ValidatingVisitor::visit()

use crate::graph::{EdgeType, Node, NodeData, NodeType};
use crate::log;
use crate::progress::{
    progress_stream, report_state, sort_by_string, ProgressOptions, ProgressRecorder,
    ProgressRecorderUnprotected, ProgressReporter, ProgressReporterUnprotected, ProgressStateMutex,
};
use crate::setup::{
    parse_progress_args, setup_common, JobWalkParams, RepoSubcommandParams, EXCLUDE_CHECK_TYPE_ARG,
    INCLUDE_CHECK_TYPE_ARG, VALIDATE,
};
use crate::state::{InternedType, StepStats, WalkState};
use crate::tail::walk_exact_tail;
use crate::walk::{
    EmptyRoute, OutgoingEdge, RepoWalkParams, RepoWalkTypeParams, StepRoute, TailingWalkVisitor,
    VisitOne, WalkVisitor,
};

use anyhow::Error;
use async_trait::async_trait;
use bonsai_hg_mapping::BonsaiHgMapping;
use clap::ArgMatches;
use cloned::cloned;
use cmdlib::args::MononokeMatches;
use context::CoreContext;
use derive_more::AddAssign;
use fbinit::FacebookInit;
use futures::{future::try_join_all, stream::TryStreamExt};
use itertools::Itertools;
use maplit::hashset;
use mercurial_types::HgChangesetId;
use mononoke_types::{ChangesetId, MPath, RepositoryId};
use phases::{Phase, Phases};
use scuba_ext::MononokeScubaSampleBuilder;
use slog::{info, warn, Logger};
use stats::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    hash::Hash,
    iter::FromIterator,
    result::Result,
    str::FromStr,
    time::Instant,
};

pub const NODES: &str = "nodes";
pub const EDGES: &str = "edges";
pub const PASS: &str = "pass";
pub const FAIL: &str = "fail";
pub const TOTAL: &str = "total";
pub const NODE_KEY: &str = "node_key";
pub const NODE_TYPE: &str = "node_type";
pub const NODE_PATH: &str = "node_path";
pub const EDGE_TYPE: &str = "edge_type";
pub const CHECK_TYPE: &str = "check_type";
pub const CHECK_FAIL: &str = "check_fail";
pub const WALK_TYPE: &str = "walk_type";
pub const REPO: &str = "repo";
pub const ERROR_MSG: &str = "error_msg";
const SRC_NODE_KEY: &str = "src_node_key";
const SRC_NODE_TYPE: &str = "src_node_type";
const SRC_NODE_PATH: &str = "src_node_path";
const VIA_NODE_KEY: &str = "via_node_key";
const VIA_NODE_TYPE: &str = "via_node_type";
const VIA_NODE_PATH: &str = "via_node_path";

define_stats! {
    prefix = "mononoke.walker.validate";
    // e.g. mononoke.walker.validate.testrepo.hg_link_node_populated.pass
    walker_validate: dynamic_timeseries("{}.{}.{}", (repo: String, check: &'static str, status: &'static str); Rate, Sum),
    last_completed: dynamic_singleton_counter("{}.{}.last_completed.{}", (repo: String, check: &'static str, status: &'static str)),
}

pub const DEFAULT_CHECK_TYPES: &[CheckType] = &[
    CheckType::ChangesetPhaseIsPublic,
    CheckType::HgLinkNodePopulated,
];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FailureInfo {
    // Where we stepped from, useful for immediate reproductions with --walk-root
    source_node: Option<Node>,
    // What the check thinks is an interesting node on the route to here (e.g. the affected changeset)
    via_node: Option<Node>,
}

impl FailureInfo {
    fn new(source_node: Option<Node>, via_node: Option<Node>) -> Self {
        Self {
            source_node,
            via_node,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum CheckStatus {
    Fail(FailureInfo),
    Pass,
}

define_type_enum! {
enum CheckType {
    ChangesetPhaseIsPublic,
    HgLinkNodePopulated,
}
}

impl CheckType {
    fn stats_key(&self) -> &'static str {
        match self {
            CheckType::ChangesetPhaseIsPublic => "bonsai_phase_is_public",
            CheckType::HgLinkNodePopulated => "hg_link_node_populated",
        }
    }
    pub fn node_type(&self) -> NodeType {
        match self {
            CheckType::ChangesetPhaseIsPublic => NodeType::PhaseMapping,
            CheckType::HgLinkNodePopulated => NodeType::HgFileNode,
        }
    }
}

impl fmt::Display for CheckType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug)]
struct CheckOutput {
    check: CheckType,
    status: CheckStatus,
}

impl CheckOutput {
    fn new(check: CheckType, status: CheckStatus) -> Self {
        Self { check, status }
    }
}

struct ValidatingVisitor {
    repo_stats_key: String,
    inner: WalkState,
    checks_by_node_type: HashMap<NodeType, HashSet<CheckType>>,
}

impl ValidatingVisitor {
    pub fn new(
        repo_stats_key: String,
        include_node_types: HashSet<NodeType>,
        include_edge_types: HashSet<EdgeType>,
        include_checks: HashSet<CheckType>,
        always_emit_edge_types: HashSet<EdgeType>,
        enable_derive: bool,
    ) -> Self {
        Self {
            repo_stats_key,
            inner: WalkState::new(
                include_node_types,
                include_edge_types,
                always_emit_edge_types,
                enable_derive,
            ),
            checks_by_node_type: include_checks
                .into_iter()
                .group_by(|c| c.node_type())
                .into_iter()
                .map(|(key, group)| (key, HashSet::from_iter(group)))
                .collect(),
        }
    }
}

#[async_trait]
impl VisitOne for ValidatingVisitor {
    fn in_chunk(&self, bcs_id: &ChangesetId) -> bool {
        self.inner.in_chunk(bcs_id)
    }

    fn needs_visit(&self, outgoing: &OutgoingEdge) -> bool {
        self.inner.needs_visit(outgoing)
    }

    async fn is_public(
        &self,
        ctx: &CoreContext,
        phases_store: &dyn Phases,
        bcs_id: &ChangesetId,
    ) -> Result<bool, Error> {
        self.inner.is_public(ctx, phases_store, bcs_id).await
    }

    async fn defer_from_hg(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        bonsai_hg_mapping: &dyn BonsaiHgMapping,
        hg_cs_id: &HgChangesetId,
    ) -> Result<Option<ChangesetId>, Error> {
        self.inner
            .defer_from_hg(ctx, repo_id, bonsai_hg_mapping, hg_cs_id)
            .await
    }
}

fn check_bonsai_phase_is_public(
    node: &Node,
    node_data: Option<&NodeData>,
    route: Option<&ValidateRoute>,
) -> CheckStatus {
    match (&node, &node_data) {
        (Node::PhaseMapping(_cs_id), Some(NodeData::PhaseMapping(Some(Phase::Public)))) => {
            CheckStatus::Pass
        }
        (Node::PhaseMapping(non_public_cs_id), Some(NodeData::PhaseMapping(_phase))) => {
            let via = route.and_then(|r| {
                for n in r.via.iter().rev() {
                    match n {
                        Node::HgChangeset(_via_hg_cs_id) => return Some(n.clone()),
                        Node::Changeset(k) => {
                            // Check for most recent non-identical changesethg
                            if &k.inner != non_public_cs_id {
                                return Some(n.clone());
                            }
                        }
                        _ => {}
                    }
                }
                return None;
            });
            CheckStatus::Fail(FailureInfo::new(route.map(|r| r.src_node.clone()), via))
        }
        _ => CheckStatus::Fail(FailureInfo::new(route.map(|r| r.src_node.clone()), None)),
    }
}

fn check_linknode_populated(
    outgoing: &[OutgoingEdge],
    route: Option<&ValidateRoute>,
) -> CheckStatus {
    if outgoing
        .iter()
        .any(|e| e.label == EdgeType::HgFileNodeToLinkedHgChangeset)
    {
        CheckStatus::Pass
    } else {
        let via = route.and_then(|r| {
            for n in r.via.iter().rev() {
                match n {
                    Node::HgChangeset(_via_hg_cs_id) => return Some(n.clone()),
                    _ => {}
                }
            }
            return None;
        });
        CheckStatus::Fail(FailureInfo::new(route.map(|r| r.src_node.clone()), via))
    }
}

#[derive(AddAssign, Clone, Copy, Default, Debug)]
struct CheckStats {
    pass: u64,
    fail: u64,
    edges: u64,
}

struct CheckData {
    checked: Vec<CheckOutput>,
    stats: CheckStats,
}

#[derive(Clone, Debug)]
struct ValidateRoute {
    src_node: Node,
    via: Vec<Node>,
}

impl ValidateRoute {
    // Keep memory usage bounded
    const MAX_VIA: usize = 2;

    fn next_route(route: Option<Self>, node: Node) -> Self {
        let mut next_via = match route {
            Some(Self {
                src_node: _src_node,
                mut via,
            }) => {
                if via.len() > ValidateRoute::MAX_VIA {
                    via.remove(0);
                }
                via
            }
            None => vec![],
        };

        // Only track changesets for the via information
        match node {
            Node::HgChangeset(_) | Node::Changeset(_) => next_via.push(node.clone()),
            _ => {}
        };
        Self {
            src_node: node,
            via: next_via,
        }
    }
}

impl StepRoute for ValidateRoute {
    fn source_node(&self) -> Option<&Node> {
        Some(&self.src_node)
    }
    fn via_node(&self) -> Option<&Node> {
        self.via.last()
    }
}

impl TailingWalkVisitor for ValidatingVisitor {
    fn start_chunk(
        &mut self,
        chunk_members: &HashSet<ChangesetId>,
    ) -> Result<HashSet<OutgoingEdge>, Error> {
        self.inner.start_chunk(chunk_members)
    }

    fn clear_state(
        &mut self,
        node_types: &HashSet<NodeType>,
        interned_types: &HashSet<InternedType>,
    ) {
        self.inner.clear_state(node_types, interned_types)
    }

    fn end_chunks(&mut self, contiguous_bounds: bool) -> Result<(), Error> {
        self.inner.end_chunks(contiguous_bounds)
    }

    fn num_deferred(&self) -> usize {
        self.inner.num_deferred()
    }
}

impl WalkVisitor<(Node, Option<CheckData>, Option<StepStats>), ValidateRoute>
    for ValidatingVisitor
{
    fn start_step(
        &self,
        ctx: CoreContext,
        route: Option<&ValidateRoute>,
        step: &OutgoingEdge,
    ) -> CoreContext {
        self.inner
            .start_step(ctx, route.map(|_| &EmptyRoute {}), step)
    }

    fn visit(
        &self,
        ctx: &CoreContext,
        resolved: OutgoingEdge,
        node_data: Option<NodeData>,
        route: Option<ValidateRoute>,
        outgoing: Vec<OutgoingEdge>,
    ) -> (
        (Node, Option<CheckData>, Option<StepStats>),
        ValidateRoute,
        Vec<OutgoingEdge>,
    ) {
        let checks_to_do: Option<&HashSet<_>> =
            self.checks_by_node_type.get(&resolved.target.get_type());
        // The incoming resolved edge counts as one
        let mut num_edges: u64 = 1;
        let mut pass = 0;
        let mut fail = 0;
        let checked: Vec<_> = checks_to_do
            .map(|set| {
                set.iter().filter_map(|check| {
                    // Lets check!
                    let status = match check {
                        CheckType::ChangesetPhaseIsPublic => check_bonsai_phase_is_public(
                            &resolved.target,
                            node_data.as_ref(),
                            route.as_ref(),
                        ),
                        CheckType::HgLinkNodePopulated => {
                            num_edges += outgoing.len() as u64;
                            check_linknode_populated(&outgoing, route.as_ref())
                        }
                    };
                    if status == CheckStatus::Pass {
                        pass += 1;
                    } else {
                        fail += 1;
                    }
                    Some(CheckOutput::new(*check, status))
                })
            })
            .into_iter()
            .flatten()
            .collect();

        STATS::walker_validate.add_value(
            num_edges as i64,
            (self.repo_stats_key.clone(), EDGES, TOTAL),
        );

        // Call inner after checks. otherwise it will prune outgoing edges we wanted to check.
        let ((node, _opt_data, opt_stats), _, outgoing) = self.inner.visit(
            &ctx,
            resolved,
            node_data,
            route.as_ref().map(|_| EmptyRoute {}),
            outgoing,
        );

        let vout = (
            node.clone(),
            if checked.is_empty() {
                None
            } else {
                Some(CheckData {
                    checked,
                    stats: CheckStats {
                        pass,
                        fail,
                        edges: num_edges,
                    },
                })
            },
            opt_stats,
        );

        (vout, ValidateRoute::next_route(route, node), outgoing)
    }

    fn defer_visit(
        &self,
        bcs_id: &ChangesetId,
        walk_item: &OutgoingEdge,
        route: Option<ValidateRoute>,
    ) -> ((Node, Option<CheckData>, Option<StepStats>), ValidateRoute) {
        let ((node, _node_data, stats), _route) =
            self.inner
                .defer_visit(bcs_id, walk_item, Some(EmptyRoute {}));
        (
            (node.clone(), None, stats),
            ValidateRoute::next_route(route, node),
        )
    }
}

fn parse_check_types(sub_m: &ArgMatches<'_>) -> Result<HashSet<CheckType>, Error> {
    let mut include_types: HashSet<CheckType> = match sub_m.values_of(INCLUDE_CHECK_TYPE_ARG) {
        None => Ok(HashSet::from_iter(DEFAULT_CHECK_TYPES.iter().cloned())),
        Some(values) => values.map(CheckType::from_str).collect(),
    }?;
    let exclude_types: HashSet<CheckType> = match sub_m.values_of(EXCLUDE_CHECK_TYPE_ARG) {
        None => Ok(HashSet::new()),
        Some(values) => values.map(CheckType::from_str).collect(),
    }?;
    include_types.retain(|x| !exclude_types.contains(x));
    Ok(include_types)
}

struct ValidateProgressState {
    logger: Logger,
    fb: FacebookInit,
    scuba_builder: MononokeScubaSampleBuilder,
    repo_stats_key: String,
    types_sorted_by_name: Vec<CheckType>,
    stats_by_type: HashMap<CheckType, CheckStats>,
    total_checks: CheckStats,
    checked_nodes: u64,
    passed_nodes: u64,
    failed_nodes: u64,
    throttle_options: ProgressOptions,
    last_update: Instant,
}

impl ValidateProgressState {
    fn new(
        logger: Logger,
        fb: FacebookInit,
        scuba_builder: MononokeScubaSampleBuilder,
        repo_stats_key: String,
        included_types: HashSet<CheckType>,
        throttle_options: ProgressOptions,
    ) -> Self {
        let types_sorted_by_name = sort_by_string(included_types);
        let now = Instant::now();
        Self {
            logger,
            fb,
            scuba_builder,
            repo_stats_key,
            types_sorted_by_name,
            stats_by_type: HashMap::new(),
            total_checks: CheckStats::default(),
            checked_nodes: 0,
            passed_nodes: 0,
            failed_nodes: 0,
            throttle_options,
            last_update: now,
        }
    }

    fn report_progress_log(&self) {
        let detail_by_type = &self
            .types_sorted_by_name
            .iter()
            .map(|t| {
                let d = CheckStats::default();
                let stats = self.stats_by_type.get(t).unwrap_or(&d);
                format!("{}:{},{}", t, stats.pass, stats.fail)
            })
            .collect::<Vec<_>>()
            .join(" ");
        info!(
            self.logger,
            #log::VALIDATE,
            "Nodes,Pass,Fail:{},{},{}; EdgesChecked:{}; CheckType:Pass,Fail Total:{},{} {}",
            self.checked_nodes,
            self.passed_nodes,
            self.failed_nodes,
            self.total_checks.edges,
            self.total_checks.pass,
            self.total_checks.fail,
            detail_by_type,
        );
    }

    fn report_progress_stats(&self) {
        // Per check type
        for (k, v) in self.stats_by_type.iter() {
            for (desc, value) in &[(PASS, v.pass), (FAIL, v.fail), (EDGES, v.edges)] {
                STATS::last_completed.set_value(
                    self.fb,
                    *value as i64,
                    (self.repo_stats_key.clone(), k.stats_key(), desc),
                );
            }
        }
        // Overall by nodes and edges
        for (stat, desc, value) in &[
            (NODES, PASS, self.passed_nodes),
            (NODES, FAIL, self.failed_nodes),
            (NODES, TOTAL, self.checked_nodes),
            (EDGES, TOTAL, self.total_checks.edges),
        ] {
            STATS::last_completed.set_value(
                self.fb,
                *value as i64,
                (self.repo_stats_key.clone(), stat, desc),
            );
        }
    }
}

fn scuba_log_node(
    n: &Node,
    scuba: &mut MononokeScubaSampleBuilder,
    type_key: &'static str,
    key_key: &'static str,
    path_key: &'static str,
) {
    scuba
        .add(type_key, Into::<&'static str>::into(n.get_type()))
        .add(key_key, n.stats_key());
    if let Some(path) = n.stats_path() {
        scuba.add(path_key, MPath::display_opt(path.as_ref()).to_string());
    }
}

pub fn add_node_to_scuba(
    source_node: Option<&Node>,
    via_node: Option<&Node>,
    n: &Node,
    scuba: &mut MononokeScubaSampleBuilder,
) {
    scuba_log_node(n, scuba, NODE_TYPE, NODE_KEY, NODE_PATH);
    if let Some(src_node) = source_node {
        scuba_log_node(src_node, scuba, SRC_NODE_TYPE, SRC_NODE_KEY, SRC_NODE_PATH);
    }
    if let Some(via_node) = via_node {
        scuba_log_node(via_node, scuba, VIA_NODE_TYPE, VIA_NODE_KEY, VIA_NODE_PATH);
    }
}

impl ProgressRecorderUnprotected<CheckData> for ValidateProgressState {
    fn set_sample_builder(&mut self, s: MononokeScubaSampleBuilder) {
        self.scuba_builder = s;
    }

    fn record_step(&mut self, n: &Node, checkdata: Option<&CheckData>) {
        self.checked_nodes += 1;
        let mut had_pass = false;
        let mut had_fail = false;
        if let Some(checkdata) = checkdata {
            // By node. One fail is enough for a Node to be failed.
            if checkdata.stats.fail > 0 {
                had_fail = true;
            } else if !had_fail && checkdata.stats.pass > 0 {
                had_pass = true;
            }
            // total
            self.total_checks += checkdata.stats;
            // By type
            for c in &checkdata.checked {
                let k = c.check;
                let stats = self.stats_by_type.entry(k).or_insert(CheckStats::default());
                match &c.status {
                    CheckStatus::Pass => {
                        stats.pass += 1;
                        STATS::walker_validate
                            .add_value(1, (self.repo_stats_key.clone(), k.stats_key(), PASS));
                    }
                    CheckStatus::Fail(failure_info) => {
                        STATS::walker_validate
                            .add_value(1, (self.repo_stats_key.clone(), k.stats_key(), FAIL));
                        stats.fail += 1;
                        // For failures log immediately
                        let mut scuba = self.scuba_builder.clone();
                        add_node_to_scuba(
                            failure_info.source_node.as_ref(),
                            failure_info.via_node.as_ref(),
                            n,
                            &mut scuba,
                        );
                        scuba
                            .add(CHECK_TYPE, k.stats_key())
                            .add(
                                CHECK_FAIL,
                                if c.status == CheckStatus::Pass { 0 } else { 1 },
                            )
                            .log();
                        for json in scuba.get_sample().to_json() {
                            warn!(self.logger, "Validation failed: {}", json)
                        }
                    }
                }
            }
        }

        if had_pass {
            self.passed_nodes += 1;
            STATS::walker_validate.add_value(1, (self.repo_stats_key.clone(), NODES, PASS));
        } else if had_fail {
            self.failed_nodes += 1;
            STATS::walker_validate.add_value(1, (self.repo_stats_key.clone(), NODES, FAIL));
        }
        STATS::walker_validate.add_value(1, (self.repo_stats_key.clone(), NODES, TOTAL));
    }
}

impl ProgressReporterUnprotected for ValidateProgressState {
    fn report_progress(&mut self) {
        self.report_progress_log();
        self.report_progress_stats();
    }

    fn report_throttled(&mut self) {
        if self.checked_nodes % self.throttle_options.sample_rate == 0 {
            let new_update = Instant::now();
            let delta_time = new_update.duration_since(self.last_update);
            if delta_time >= self.throttle_options.interval {
                self.report_progress_log();
                self.last_update = new_update;
            }
        }
    }
}

#[derive(Clone)]
struct ValidateCommand {
    include_check_types: HashSet<CheckType>,
    progress_options: ProgressOptions,
}

impl ValidateCommand {
    fn apply_repo(&mut self, repo_params: &RepoWalkParams) {
        self.include_check_types
            .retain(|t| repo_params.include_node_types.contains(&t.node_type()));
    }
}

// Subcommand entry point for validation of mononoke commit graph and dependent data
pub async fn validate<'a>(
    fb: FacebookInit,
    logger: Logger,
    matches: &'a MononokeMatches<'a>,
    sub_m: &'a ArgMatches<'a>,
) -> Result<(), Error> {
    let (job_params, per_repo) = setup_common(VALIDATE, fb, &logger, None, matches, sub_m).await?;

    let command = ValidateCommand {
        include_check_types: parse_check_types(sub_m)?,
        progress_options: parse_progress_args(&sub_m),
    };

    let mut all_walks = Vec::new();
    for (sub_params, repo_params) in per_repo {
        cloned!(mut command, job_params);

        command.apply_repo(&repo_params);

        let walk = run_one(fb, job_params, sub_params, repo_params, command);
        all_walks.push(walk);
    }
    try_join_all(all_walks).await.map(|_| ())
}

async fn run_one(
    fb: FacebookInit,
    job_params: JobWalkParams,
    sub_params: RepoSubcommandParams,
    repo_params: RepoWalkParams,
    command: ValidateCommand,
) -> Result<(), Error> {
    info!(
        repo_params.logger,
        #log::VALIDATE,
        "Performing check types {:?}",
        sort_by_string(&command.include_check_types)
    );

    let validate_progress_state = ProgressStateMutex::new(ValidateProgressState::new(
        repo_params.logger.clone(),
        fb,
        repo_params.scuba_builder.clone(),
        repo_params.repo.name().clone(),
        command.include_check_types.clone(),
        command.progress_options,
    ));

    cloned!(job_params.quiet, sub_params.progress_state);
    let make_sink = move |ctx: &CoreContext, repo_params: &RepoWalkParams| {
        cloned!(ctx);
        validate_progress_state.set_sample_builder(repo_params.scuba_builder.clone());
        async move |walk_output| {
            cloned!(ctx, progress_state, validate_progress_state);
            let walk_progress =
                progress_stream(quiet, &progress_state, walk_output).map_ok(|(n, d, s)| {
                    // swap stats and data round
                    (n, s, d)
                });

            let validate_progress = progress_stream(quiet, &validate_progress_state, walk_progress);

            report_state(ctx, validate_progress).await?;
            progress_state.report_progress();
            validate_progress_state.report_progress();
            Ok(())
        }
    };

    let always_emit_edge_types =
        HashSet::from_iter(vec![EdgeType::HgFileNodeToLinkedHgChangeset].into_iter());

    let stateful_visitor = ValidatingVisitor::new(
        repo_params.repo.name().clone(),
        repo_params.include_node_types.clone(),
        repo_params.include_edge_types.clone(),
        command.include_check_types,
        always_emit_edge_types.clone(),
        job_params.enable_derive,
    );

    let type_params = RepoWalkTypeParams {
        required_node_data_types: hashset![NodeType::PhaseMapping],
        always_emit_edge_types,
        keep_edge_paths: false,
    };

    walk_exact_tail(
        fb,
        job_params,
        repo_params,
        type_params,
        sub_params.tail_params,
        stateful_visitor,
        make_sink,
    )
    .await
}
