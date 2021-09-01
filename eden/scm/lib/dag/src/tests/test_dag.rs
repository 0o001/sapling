/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::ops::CheckIntegrity;
use crate::ops::DagAddHeads;
use crate::ops::DagAlgorithm;
use crate::ops::DagExportCloneData;
use crate::ops::DagImportCloneData;
use crate::ops::DagImportPullData;
use crate::ops::DagPersistent;
use crate::ops::DagPullFastForwardMasterData;
use crate::ops::IdConvert;
use crate::protocol;
use crate::protocol::RemoteIdConvertProtocol;
use crate::render::render_namedag;
use crate::Group;
use crate::Level;
use crate::NameDag;
use crate::Result;
use crate::Vertex;
use futures::StreamExt;
use nonblocking::non_blocking;
use nonblocking::non_blocking_result;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::debug;

/// Dag structure for testing purpose.
pub struct TestDag {
    pub dag: NameDag,
    pub seg_size: usize,
    pub dir: tempfile::TempDir,
    pub output: Arc<Mutex<Vec<String>>>,
}

impl TestDag {
    /// Creates a `TestDag` for testing.
    /// Side effect of the `TestDag` will be removed on drop.
    pub fn new() -> Self {
        Self::new_with_segment_size(3)
    }

    /// Crates a `TestDag` using the given ASCII.
    ///
    /// This is just `new`, followed by `drawdag`, with an extra rule that
    /// comments like "# master: M" at the end can be used to specify master
    /// heads .
    pub fn draw(text: &str) -> Self {
        let mut dag = Self::new();
        let mut split = text.split("# master:");
        let text = split.next().unwrap_or("");
        let master = match split.next() {
            Some(t) => t.split_whitespace().collect::<Vec<_>>(),
            None => Vec::new(),
        };
        dag.drawdag(text, &master);
        dag
    }

    /// Creates a `TestDag` with a specific segment size.
    pub fn new_with_segment_size(seg_size: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let dag = NameDag::open(dir.path().join("n")).unwrap();
        Self {
            dir,
            dag,
            seg_size,
            output: Default::default(),
        }
    }

    /// Reopen the dag. Drop in-memory state including caches.
    pub fn reopen(&mut self) {
        let mut dag = NameDag::open(self.dir.path().join("n")).unwrap();
        dag.set_remote_protocol(self.dag.get_remote_protocol());
        self.dag = dag;
    }

    /// Add vertexes to the graph. Does not resolve vertexes remotely.
    pub fn drawdag(&mut self, text: &str, master_heads: &[&str]) {
        self.drawdag_with_limited_heads(text, master_heads, None);
    }

    /// Add vertexes to the graph. Async version that might resolve vertexes
    /// remotely on demand.
    pub async fn drawdag_async(&mut self, text: &str, master_heads: &[&str]) {
        // Do not call self.validate to avoid fetching vertexes remotely.
        self.drawdag_with_limited_heads_async(text, master_heads, None, false)
            .await
    }

    /// Add vertexes to the graph.
    ///
    /// If `heads` is set, ignore part of the graph. Only consider specified
    /// heads.
    pub fn drawdag_with_limited_heads(
        &mut self,
        text: &str,
        master_heads: &[&str],
        heads: Option<&[&str]>,
    ) {
        non_blocking(self.drawdag_with_limited_heads_async(text, master_heads, heads, true))
            .unwrap()
    }

    pub async fn drawdag_with_limited_heads_async(
        &mut self,
        text: &str,
        master_heads: &[&str],
        heads: Option<&[&str]>,
        validate: bool,
    ) {
        let (all_heads, parent_func) = get_heads_and_parents_func_from_ascii(text);
        let heads = match heads {
            Some(heads) => heads
                .iter()
                .map(|s| Vertex::copy_from(s.as_bytes()))
                .collect(),
            None => all_heads,
        };
        self.dag.dag.set_new_segment_size(self.seg_size);
        self.dag.add_heads(&parent_func, &heads).await.unwrap();
        if validate {
            self.validate().await;
        }
        assert_eq!(self.dag.check_segments().await.unwrap(), [] as [String; 0]);
        let master_heads = master_heads
            .iter()
            .map(|s| Vertex::copy_from(s.as_bytes()))
            .collect::<Vec<_>>();
        let need_flush = !master_heads.is_empty();
        if need_flush {
            self.dag.flush(&master_heads).await.unwrap();
        }
        if validate {
            self.validate().await;
        }
        assert_eq!(self.dag.check_segments().await.unwrap(), [] as [String; 0]);
    }

    /// Replace ASCII with Ids in the graph.
    pub fn annotate_ascii(&self, text: &str) -> String {
        self.dag.map.replace(text)
    }

    /// Render the segments.
    pub fn render_segments(&self) -> String {
        format!("{:?}", &self.dag.dag)
    }

    /// Render the graph.
    pub fn render_graph(&self) -> String {
        render_namedag(&self.dag, |v| {
            Some(
                non_blocking_result(self.dag.vertex_id(v.clone()))
                    .unwrap()
                    .to_string(),
            )
        })
        .unwrap()
    }

    /// Use this DAG as the "server", return the "client" Dag that has lazy Vertexes.
    pub async fn client(&self) -> TestDag {
        let mut client = TestDag::new();
        client.set_remote(&self);
        client
    }

    /// Update remote protocol to use the (updated) server graph.
    pub fn set_remote(&mut self, server_dag: &Self) {
        let remote = server_dag.remote_protocol(self.output.clone());
        self.dag.set_remote_protocol(remote);
    }

    /// Similar to `client`, but also clone the Dag from the server.
    pub async fn client_cloned_data(&self) -> TestDag {
        let mut client = self.client().await;
        let data = self.dag.export_clone_data().await.unwrap();
        client.dag.import_clone_data(data).await.unwrap();
        client
    }

    /// Pull from the server Dag using the master fast forward fast path.
    pub async fn pull_ff_master(
        &mut self,
        server: &Self,
        old_master: impl Into<Vertex>,
        new_master: impl Into<Vertex>,
    ) -> Result<()> {
        self.set_remote(server);
        let data = server
            .dag
            .pull_fast_forward_master(old_master.into(), new_master.into())
            .await?;
        debug!("pull_ff data: {:?}", &data);
        self.dag.import_pull_data(data).await?;
        Ok(())
    }

    /// Remote protocol used to resolve Id <-> Vertex remotely using the test dag
    /// as the "server".
    ///
    /// Logs of the remote access will be written to `output`.
    pub fn remote_protocol(
        &self,
        output: Arc<Mutex<Vec<String>>>,
    ) -> Arc<dyn RemoteIdConvertProtocol> {
        let remote = ProtocolMonitor {
            inner: Box::new(self.dag.try_snapshot().unwrap()),
            output,
        };
        Arc::new(remote)
    }

    /// Describe segments at the given level and group as a string.
    pub fn debug_segments(&self, level: Level, group: Group) -> String {
        let lines = crate::namedag::debug_segments_by_level_group(
            &self.dag.dag,
            &self.dag.map,
            level,
            group,
        );
        lines
            .iter()
            .map(|l| format!("\n        {}", l))
            .collect::<Vec<String>>()
            .concat()
    }

    /// Output of remote protocols since the last call.
    pub fn output(&self) -> Vec<String> {
        let mut result = Vec::new();
        let mut output = self.output.lock();
        std::mem::swap(&mut result, &mut *output);
        result
    }

    /// Check that a vertex exists locally.
    pub fn contains_vertex_locally(&self, name: impl Into<Vertex>) -> bool {
        non_blocking_result(self.dag.contains_vertex_name_locally(&[name.into()])).unwrap()[0]
    }

    async fn validate(&self) {
        // All vertexes should be accessible, and round-trip through IdMap.
        let mut iter = self.dag.all().await.unwrap().iter().await.unwrap();
        while let Some(v) = iter.next().await {
            let v = v.unwrap();
            let id = self.dag.vertex_id(v.clone()).await.unwrap();
            let v2 = self.dag.vertex_name(id).await.unwrap();
            assert_eq!(v, v2);
        }
    }
}

pub(crate) struct ProtocolMonitor {
    pub(crate) inner: Box<dyn RemoteIdConvertProtocol>,
    pub(crate) output: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl RemoteIdConvertProtocol for ProtocolMonitor {
    async fn resolve_names_to_relative_paths(
        &self,
        heads: Vec<Vertex>,
        names: Vec<Vertex>,
    ) -> Result<Vec<(protocol::AncestorPath, Vec<Vertex>)>> {
        let msg = format!("resolve names: {:?}, heads: {:?}", &names, &heads);
        self.output.lock().push(msg);
        self.inner
            .resolve_names_to_relative_paths(heads, names)
            .await
    }

    async fn resolve_relative_paths_to_names(
        &self,
        paths: Vec<protocol::AncestorPath>,
    ) -> Result<Vec<(protocol::AncestorPath, Vec<Vertex>)>> {
        let msg = format!("resolve paths: {:?}", &paths);
        self.output.lock().push(msg);
        self.inner.resolve_relative_paths_to_names(paths).await
    }
}

fn get_heads_and_parents_func_from_ascii(
    text: &str,
) -> (Vec<Vertex>, HashMap<Vertex, Vec<Vertex>>) {
    let parents = drawdag::parse(&text);
    let mut heads = parents
        .keys()
        .collect::<HashSet<_>>()
        .difference(&parents.values().flat_map(|ps| ps.into_iter()).collect())
        .map(|&v| Vertex::copy_from(v.as_bytes()))
        .collect::<Vec<_>>();
    heads.sort();
    let v = |s: String| Vertex::copy_from(s.as_bytes());
    let parents = parents
        .into_iter()
        .map(|(k, vs)| (v(k), vs.into_iter().map(v).collect()))
        .collect();
    (heads, parents)
}
