/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
use configmodel::Config;
use configmodel::ConfigExt;
use io::IO;
use manifest_tree::ReadTreeManifest;
use parking_lot::Mutex;
use pathmatcher::DynMatcher;
use pathmatcher::Matcher;
use repolock::RepoLocker;
use storemodel::ReadFileContents;
use treestate::filestate::StateFlags;
use treestate::tree::VisitorResult;
use treestate::treestate::TreeState;
use types::RepoPathBuf;
use vfs::VFS;

use crate::filechangedetector::FileChangeDetector;
use crate::filechangedetector::FileChangeResult;
use crate::filechangedetector::ResolvedFileChangeResult;
use crate::filesystem::PendingChange;
use crate::filesystem::PendingChanges as PendingChangesTrait;
use crate::metadata;
use crate::metadata::HgModifiedTime;
use crate::util::dirstate_write_time_override;
use crate::util::maybe_flush_treestate;
use crate::walker::WalkEntry;
use crate::walker::Walker;
use crate::workingcopy::WorkingCopy;

type ArcReadFileContents = Arc<dyn ReadFileContents<Error = anyhow::Error> + Send + Sync>;
type ArcReadTreeManifest = Arc<dyn ReadTreeManifest + Send + Sync>;

pub struct PhysicalFileSystem {
    // TODO: Make this an Arc<Mutex<VFS>> so we can persist the vfs pathauditor cache
    vfs: VFS,
    tree_resolver: ArcReadTreeManifest,
    store: ArcReadFileContents,
    treestate: Arc<Mutex<TreeState>>,
    locker: Arc<RepoLocker>,
}

impl PhysicalFileSystem {
    pub fn new(
        vfs: VFS,
        tree_resolver: ArcReadTreeManifest,
        store: ArcReadFileContents,
        treestate: Arc<Mutex<TreeState>>,
        locker: Arc<RepoLocker>,
    ) -> Result<Self> {
        Ok(PhysicalFileSystem {
            vfs,
            tree_resolver,
            store,
            treestate,
            locker,
        })
    }
}

impl PendingChangesTrait for PhysicalFileSystem {
    fn pending_changes(
        &self,
        matcher: DynMatcher,
        ignore_matcher: DynMatcher,
        ignore_dirs: Vec<PathBuf>,
        include_ignored: bool,
        last_write: SystemTime,
        config: &dyn Config,
        _io: &IO,
    ) -> Result<Box<dyn Iterator<Item = Result<PendingChange>>>> {
        let root = self.vfs.root().to_path_buf();
        let ident = identity::must_sniff_dir(&root)?;
        let walker = Walker::new(
            root,
            ident.dot_dir().to_string(),
            ignore_dirs,
            matcher.clone(),
            false,
        )?;
        let manifests =
            WorkingCopy::current_manifests(&self.treestate.lock(), &self.tree_resolver)?;
        let file_change_detector = FileChangeDetector::new(
            self.vfs.clone(),
            last_write.try_into()?,
            manifests[0].clone(),
            self.store.clone(),
            config.get_opt("workingcopy", "worker-count")?,
        );
        let pending_changes = PendingChanges {
            walker,
            matcher,
            ignore_matcher,
            include_ignored,
            treestate: self.treestate.clone(),
            stage: PendingChangesStage::Walk,
            seen: HashSet::new(),
            tree_iter: None,
            lookup_iter: None,
            file_change_detector: Some(file_change_detector),
            update_mtime: Vec::new(),
            locker: self.locker.clone(),
            dirstate_write_time: dirstate_write_time_override(config),
            vfs: self.vfs.clone(),
        };
        Ok(Box::new(pending_changes))
    }
}

pub struct PendingChanges<M: Matcher + Clone + Send + Sync + 'static> {
    walker: Walker<M>,
    matcher: M,
    ignore_matcher: M,
    include_ignored: bool,
    treestate: Arc<Mutex<TreeState>>,
    stage: PendingChangesStage,
    seen: HashSet<RepoPathBuf>,
    tree_iter: Option<Box<dyn Iterator<Item = Result<PendingChange>> + Send>>,
    lookup_iter: Option<Box<dyn Iterator<Item = Result<ResolvedFileChangeResult>> + Send>>,
    file_change_detector: Option<FileChangeDetector>,
    update_mtime: Vec<(RepoPathBuf, HgModifiedTime)>,
    locker: Arc<RepoLocker>,
    dirstate_write_time: Option<i64>,
    vfs: VFS,
}

#[derive(PartialEq)]
enum PendingChangesStage {
    Walk,
    IterateTree,
    Lookups,
    Finished,
}

impl PendingChangesStage {
    pub fn next(&self) -> PendingChangesStage {
        match self {
            PendingChangesStage::Walk => PendingChangesStage::IterateTree,
            PendingChangesStage::IterateTree => PendingChangesStage::Lookups,
            PendingChangesStage::Lookups => PendingChangesStage::Finished,
            PendingChangesStage::Finished => PendingChangesStage::Finished,
        }
    }
}

impl<M: Matcher + Clone + Send + Sync + 'static> PendingChanges<M> {
    fn next_walk(&mut self) -> Result<Option<PendingChange>> {
        loop {
            match self.walker.next() {
                Some(Ok(WalkEntry::File(mut path, metadata))) => {
                    if self.include_ignored && self.ignore_matcher.matches_file(&path)? {
                        return Ok(Some(PendingChange::Ignored(path)));
                    }

                    let mut ts = self.treestate.lock();

                    // On case insensitive systems, normalize the path so
                    // duplicate paths with different case can be detected in
                    // the seen set, but only if the dirstate entry hasn't been
                    // deleted.
                    let (normalized, ts_state) = ts.normalize_path_and_get(path.as_ref())?;
                    if normalized != path.as_byte_slice()
                        && ts_state
                            .as_ref()
                            .map_or(false, |s| s.state.intersects(StateFlags::EXIST_NEXT))
                    {
                        path = RepoPathBuf::from_utf8(normalized.into_owned())?;
                    }
                    self.seen.insert(path.clone());
                    let changed = self
                        .file_change_detector
                        .as_mut()
                        .unwrap()
                        .has_changed_with_fresh_metadata(metadata::File {
                            path,
                            ts_state,
                            fs_meta: Some(Some(metadata.into())),
                        })?;

                    if let FileChangeResult::Yes(change_type) = changed {
                        return Ok(Some(change_type));
                    }
                }
                Some(Ok(WalkEntry::Directory(_))) => {
                    // Shouldn't happen since we don't request directories.
                }
                Some(Err(e)) => {
                    return Err(e);
                }
                None => {
                    return Ok(None);
                }
            };
        }
    }

    fn next_tree(&mut self) -> Option<Result<PendingChange>> {
        if self.tree_iter.is_none() {
            self.tree_iter = Some(Box::new(self.get_tree_entries().into_iter()));
        }

        self.tree_iter.as_mut().unwrap().next()
    }

    fn get_tree_entries(&mut self) -> Vec<Result<PendingChange>> {
        let tracked = match self.get_tracked_from_p1() {
            Err(e) => return vec![Err(e)],
            Ok(tracked) => tracked,
        };
        let mut ts = self.treestate.lock();

        tracked
            .into_iter()
            .filter_map(|mut path| {
                let normalized = match ts.normalize_path(path.as_ref()) {
                    Ok(path) => path,
                    Err(e) => return Some(Err(e)),
                };
                if normalized != path.as_byte_slice() {
                    path = match RepoPathBuf::from_utf8(normalized.into_owned()) {
                        Ok(path) => path,
                        Err(e) => return Some(Err(e.into())),
                    };
                }

                // Skip this path if we've seen it or it doesn't match the matcher.
                if self.seen.contains(&path) {
                    return None;
                } else {
                    match self.matcher.matches_file(&path) {
                        Err(e) => {
                            return Some(Err(e));
                        }
                        Ok(false) => return None,
                        Ok(true) => {}
                    }
                }

                // This path is EXIST_P1 but not on disk - emit deleted event.
                Some(Ok(PendingChange::Deleted(path.to_owned())))
            })
            .collect()
    }

    /// Returns the files in the treestate that are from p1.
    /// We only care about files from p1 because pending_changes is relative to p1.
    fn get_tracked_from_p1(&self) -> Result<Vec<RepoPathBuf>> {
        let mut result = Vec::new();
        let mask = StateFlags::EXIST_P1;

        self.treestate.lock().visit(
            &mut |components, _| {
                let path = components.concat();
                let path = RepoPathBuf::from_utf8(path)?;
                result.push(path);
                Ok(VisitorResult::NotChanged)
            },
            &|_path, dir| match dir.get_aggregated_state() {
                None => true,
                Some(state) => state.union.intersects(mask),
            },
            &|_path, file| file.state.intersects(mask),
        )?;
        Ok(result)
    }

    fn next_lookup(&mut self) -> Option<Result<PendingChange>> {
        loop {
            let next = self
                .lookup_iter
                .get_or_insert_with(|| {
                    Box::new(self.file_change_detector.take().unwrap().into_iter())
                })
                .next()?;

            match next {
                Ok(ResolvedFileChangeResult::Yes(change_type)) => {
                    return Some(Ok(change_type));
                }
                Ok(ResolvedFileChangeResult::No((path, fs_meta))) => {
                    if let Some(mtime) = fs_meta.and_then(|m| m.mtime()) {
                        self.update_mtime.push((path, mtime));
                    }
                    continue;
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl<M: Matcher + Clone + Send + Sync + 'static> Iterator for PendingChanges<M> {
    type Item = Result<PendingChange>;

    fn next(&mut self) -> Option<Self::Item> {
        // TODO: Try to make this into a chain instead of a manual state machine
        loop {
            let change = match self.stage {
                PendingChangesStage::Walk => self.next_walk().transpose(),
                PendingChangesStage::IterateTree => self.next_tree(),
                PendingChangesStage::Lookups => self.next_lookup(),
                PendingChangesStage::Finished => None,
            };

            if change.is_some() {
                return change;
            }

            self.stage = self.stage.next();
            if self.stage == PendingChangesStage::Finished {
                if let Err(err) = self.update_treestate_mtimes() {
                    return Some(Err(err));
                }

                return None;
            }
        }
    }
}

impl<M: Matcher + Clone + Send + Sync + 'static> PendingChanges<M> {
    fn update_treestate_mtimes(&mut self) -> Result<()> {
        let mut ts = self.treestate.lock();
        let was_dirty = ts.dirty();

        for (path, mtime) in self.update_mtime.drain(..) {
            if let Some(state) = ts.get(&path)? {
                if let Ok(mtime) = mtime.try_into() {
                    let mut state = state.clone();
                    state.mtime = mtime;
                    ts.insert(&path, &state)?;
                }
            }
        }

        // Don't flush treestate if it was already dirty. If we are inside a
        // Python transaction with uncommitted, substantial dirstate changes,
        // those changes should not be written out until the transaction
        // finishes.
        if !was_dirty {
            maybe_flush_treestate(
                self.vfs.root(),
                &mut ts,
                &self.locker,
                self.dirstate_write_time.clone(),
            )?;
        }

        Ok(())
    }
}
