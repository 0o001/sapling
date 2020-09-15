/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use fbinit::FacebookInit;
use scribe_ext::Scribe;
use scuba_ext::ScubaSampleBuilder;
use slog::{o, Drain, Level, Logger};
use slog_glog_fmt::default_drain;
use sshrelay::Metadata;
use tracing::TraceContext;

use crate::logging::{LoggingContainer, SamplingKey};
use crate::perf_counters::PerfCounters;
use crate::session::{SessionClass, SessionContainer};

#[derive(Clone)]
pub struct CoreContext {
    pub fb: FacebookInit,
    session: SessionContainer,
    logging: LoggingContainer,
}

impl CoreContext {
    pub fn new_with_logger(fb: FacebookInit, logger: Logger) -> Self {
        let session = SessionContainer::new_with_defaults(fb);
        session.new_context(logger, ScubaSampleBuilder::with_discard())
    }

    // Context for bulk processing like scrubbing or bulk backfilling
    pub fn new_bulk_with_logger(fb: FacebookInit, logger: Logger) -> Self {
        let session = SessionContainer::builder(fb)
            .session_class(SessionClass::Background)
            .build();
        session.new_context(logger, ScubaSampleBuilder::with_discard())
    }

    pub fn test_mock(fb: FacebookInit) -> Self {
        let session = SessionContainer::new_with_defaults(fb);

        Self::test_mock_session(session)
    }

    pub fn test_mock_class(fb: FacebookInit, session_class: SessionClass) -> Self {
        let session = SessionContainer::builder(fb)
            .session_class(session_class)
            .build();

        Self::test_mock_session(session)
    }

    pub fn test_mock_session(session: SessionContainer) -> Self {
        let drain = default_drain().filter_level(Level::Debug).ignore_res();
        let logger = Logger::root(drain, o![]);
        session.new_context(logger, ScubaSampleBuilder::with_discard())
    }

    /// Create a new CoreContext, with a reset LoggingContainer. This is useful to reset perf
    /// counters. The existing CoreContext is unaffected.
    pub fn clone_and_reset(&self) -> Self {
        self.session
            .new_context(self.logger().clone(), self.scuba().clone())
    }

    pub fn clone_and_sample(&self, sampling_key: SamplingKey) -> Self {
        Self {
            fb: self.fb,
            session: self.session.clone(),
            logging: self.logging.clone_and_sample(sampling_key),
        }
    }

    pub fn with_mutated_scuba(
        &self,
        sample: impl FnOnce(ScubaSampleBuilder) -> ScubaSampleBuilder,
    ) -> Self {
        self.session
            .new_context(self.logger().clone(), sample(self.scuba().clone()))
    }

    pub(crate) fn new_with_containers(
        fb: FacebookInit,
        logging: LoggingContainer,
        session: SessionContainer,
    ) -> Self {
        Self {
            fb,
            logging,
            session,
        }
    }

    pub fn logger(&self) -> &Logger {
        &self.logging.logger()
    }

    pub fn sampling_key(&self) -> Option<&SamplingKey> {
        self.logging.sampling_key()
    }

    pub fn scuba(&self) -> &ScubaSampleBuilder {
        &self.logging.scuba()
    }

    pub fn perf_counters(&self) -> &PerfCounters {
        &self.logging.perf_counters()
    }

    pub fn trace(&self) -> &TraceContext {
        &self.session.trace()
    }

    pub fn metadata(&self) -> &Metadata {
        &self.session.metadata()
    }

    pub fn session(&self) -> &SessionContainer {
        &self.session
    }

    pub fn scribe(&self) -> &Scribe {
        self.logging.scribe()
    }
}
