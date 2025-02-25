/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

import type {RepoRelativePath} from './types';
import type {SetterOrUpdater} from 'recoil';
import type {Hash, RepoPath} from 'shared/types/common';
import type {ExportFile, ImportCommit} from 'shared/types/stack';

import clientToServerAPI from './ClientToServerAPI';
import {t} from './i18n';
import {treeWithPreviews, uncommittedChangesWithPreviews} from './previews';
import {clearOnCwdChange} from './recoilUtils';
import {ChunkSelectState} from './stackEdit/chunkSelectState';
import {assert} from './utils';
import Immutable from 'immutable';
import {useRecoilState, useRecoilValue, atom} from 'recoil';
import {RateLimiter} from 'shared/RateLimiter';
import {SelfUpdate} from 'shared/immutableExt';

type SingleFileSelection =
  | false /* not selected */
  | true /* selected, default */
  | ChunkSelectState /* maybe partially selected */;

type PartialSelectionProps = {
  /** Explicitly set selection. */
  fileMap: Immutable.Map<RepoRelativePath, SingleFileSelection>;
  /** For files not in fileMap, whether they are selected or not. */
  selectByDefault: boolean;
};
const PartialSelectionRecord = Immutable.Record<PartialSelectionProps>({
  fileMap: Immutable.Map(),
  selectByDefault: true,
});
type PartialSelectionRecord = Immutable.RecordOf<PartialSelectionProps>;

/**
 * Selection of partial changes made by a commit.
 *
 * Intended to be useful for both concrete commits and the `wdir()` virtual commit.
 * This class does not handle the differences between `wdir()` and concrete commits,
 * like how to load the file content, and how to get the list of changed files.
 * Those differences are handled at a higher level.
 */
export class PartialSelection extends SelfUpdate<PartialSelectionRecord> {
  constructor(record: PartialSelectionRecord) {
    super(record);
  }

  /** Empty selection. */
  static empty(props: {selectByDefault?: boolean}): PartialSelection {
    return new PartialSelection(PartialSelectionRecord(props));
  }

  /** Explicitly select a file. */
  select(path: RepoRelativePath): PartialSelection {
    return new PartialSelection(this.inner.setIn(['fileMap', path], true));
  }

  /** Explicitly deselect a file. */
  deselect(path: RepoRelativePath): PartialSelection {
    return new PartialSelection(this.inner.setIn(['fileMap', path], false));
  }

  /** Reset to the "default" state. Useful for commit/amend. */
  clear(): PartialSelection {
    return new PartialSelection(this.inner.set('fileMap', Immutable.Map()));
  }

  /** Start chunk selection for the given file. */
  startChunkSelect(
    path: RepoRelativePath,
    a: string,
    b: string,
    selected: boolean | string,
  ): PartialSelection {
    const chunkState = ChunkSelectState.fromText(a, b, selected);
    return new PartialSelection(this.inner.setIn(['fileMap', path], chunkState));
  }

  /** Edit chunk selection for a file. */
  editChunkSelect(
    path: RepoRelativePath,
    newValue: ((chunkState: ChunkSelectState) => ChunkSelectState) | ChunkSelectState,
  ): PartialSelection {
    const chunkState = this.inner.fileMap.get(path);
    assert(
      chunkState instanceof ChunkSelectState,
      'PartialSelection.editChunkSelect() called without startChunkEdit',
    );
    const newChunkState = typeof newValue === 'function' ? newValue(chunkState) : newValue;
    return new PartialSelection(this.inner.setIn(['fileMap', path], newChunkState));
  }

  getSelection(path: RepoRelativePath): SingleFileSelection {
    const record = this.inner;
    return record.fileMap.get(path) ?? record.selectByDefault;
  }

  /**
   * Return true if a file is selected, false if deselected,
   * or a string with the edited content.
   * Even if the file is being chunk edited, this function might
   * still return true or false.
   */
  getSimplifiedSelection(path: RepoRelativePath): boolean | string {
    const selected = this.getSelection(path);
    if (selected === true || selected === false) {
      return selected;
    }
    const chunkState: ChunkSelectState = selected;
    const text = chunkState.getSelectedText();
    if (text === chunkState.a) {
      return false;
    }
    if (text === chunkState.b) {
      return true;
    }
    return text;
  }

  isFullyOrPartiallySelected(path: RepoRelativePath): boolean {
    return this.getSimplifiedSelection(path) !== false;
  }

  isPartiallySelected(path: RepoRelativePath): boolean {
    return typeof this.getSimplifiedSelection(path) !== 'boolean';
  }

  isFullySelected(path: RepoRelativePath): boolean {
    return this.getSimplifiedSelection(path) === true;
  }

  isDeselected(path: RepoRelativePath): boolean {
    return this.getSimplifiedSelection(path) === false;
  }

  isEverythingSelected(getAllPaths: () => Array<RepoRelativePath>): boolean {
    const record = this.inner;
    const paths = record.selectByDefault ? record.fileMap.keySeq() : getAllPaths();
    return paths.every(p => this.getSimplifiedSelection(p) === true);
  }

  isNothingSelected(getAllPaths: () => Array<RepoRelativePath>): boolean {
    const record = this.inner;
    const paths = record.selectByDefault ? getAllPaths() : record.fileMap.keySeq();
    return paths.every(p => this.getSimplifiedSelection(p) === false);
  }

  /**
   * Produce a `ImportStack['files']` useful for the `debugimportstack` command
   * to create commits.
   *
   * `allPaths` provides extra file paths to be considered. This is useful
   * when we only track "deselected files".
   */
  calculateImportStackFiles(
    allPaths: Array<RepoRelativePath>,
    inverse = false,
  ): ImportCommit['files'] {
    const files: ImportCommit['files'] = {};
    // Process files in the fileMap. Note: this map might only contain the "deselected"
    // files, depending on selectByDefault.
    const fileMap = this.inner.fileMap;
    fileMap.forEach((fileSelection, path) => {
      if (fileSelection instanceof ChunkSelectState) {
        const text = inverse ? fileSelection.getInverseText() : fileSelection.getSelectedText();
        if (inverse || text !== fileSelection.a) {
          // The file is edited. Use the changed content.
          files[path] = {data: text, copyFrom: '.', flags: '.'};
        }
      } else if (fileSelection === true) {
        // '.' can be used for both inverse = true and false.
        // - For inverse = true, '.' is used with the 'write' debugimportstack command.
        //   The 'write' command treats '.' as "working parent" to "revert" changes.
        // - For inverse = false, '.' is used with the 'commit' or 'amend' debugimportstack
        //   commands. They treat '.' as "working copy" to "commit/amend" changes.
        files[path] = '.';
      }
    });
    // Process files outside the fileMap.
    allPaths.forEach(path => {
      if (!fileMap.has(path) && this.getSimplifiedSelection(path) !== false) {
        files[path] = '.';
      }
    });
    return files;
  }

  /** If any file is partially selected. */
  hasChunkSelection(): boolean {
    return this.inner.fileMap
      .keySeq()
      .some(p => typeof this.getSimplifiedSelection(p) !== 'boolean');
  }
}

/** Default: select all files. */
const defaultUncommittedPartialSelection = PartialSelection.empty({
  selectByDefault: true,
});

/** PartialSelection for `wdir()`. See `UseUncommittedSelection` for the public API. */
const uncommittedSelection = atom<PartialSelection>({
  key: 'uncommittedSelection',
  default: defaultUncommittedPartialSelection,
  effects: [clearOnCwdChange()],
});

/** PartialSelection for `wdir()` that handles loading file contents. */
export class UseUncommittedSelection {
  // Persist across `UseUncommittedSelection` life cycles.
  // Not an atom so updating the cache does not trigger re-render.
  static fileContentCache: {
    wdirHash: Hash;
    files: Map<RepoPath, ExportFile | null>;
    parentFiles: Map<RepoPath, ExportFile | null>;
    asyncLoadingLock: RateLimiter;
  } = {
    wdirHash: '',
    files: new Map(),
    parentFiles: new Map(),
    asyncLoadingLock: new RateLimiter(1),
  };

  constructor(
    public selection: PartialSelection,
    private setSelection: SetterOrUpdater<PartialSelection>,
    wdirHash: Hash,
    private getPaths: () => Array<RepoRelativePath>,
  ) {
    const cache = UseUncommittedSelection.fileContentCache;
    if (wdirHash !== cache.wdirHash) {
      // Invalidate existing cache when `.` changes.
      cache.files.clear();
      cache.parentFiles.clear();
      cache.wdirHash = wdirHash;
    }
  }

  /** Explicitly select a file. */
  select(...paths: Array<RepoRelativePath>) {
    let newSelection = this.selection;
    for (const path of paths) {
      newSelection = newSelection.select(path);
    }
    this.setSelection(newSelection);
  }

  selectAll() {
    const newSelection = defaultUncommittedPartialSelection;
    this.setSelection(newSelection);
  }

  /** Explicitly deselect a file. Also drops the related file content cache. */
  deselect(...paths: Array<RepoRelativePath>) {
    let newSelection = this.selection;
    const cache = UseUncommittedSelection.fileContentCache;
    for (const path of paths) {
      cache.files.delete(path);
      newSelection = newSelection.deselect(path);
    }
    this.setSelection(newSelection);
  }

  deselectAll() {
    let newSelection = this.selection;
    this.getPaths().forEach(path => (newSelection = newSelection.deselect(path)));
    this.setSelection(newSelection);
  }

  /** Restore to the default selection (select all). */
  clear() {
    const newSelection = this.selection.clear();
    this.setSelection(newSelection);
  }

  /**
   * Get the chunk select state for the given path.
   * The file content will be loaded on demand.
   */
  getChunkSelect(path: RepoRelativePath): ChunkSelectState | Promise<ChunkSelectState> {
    const fileSelection = this.selection.inner.fileMap.get(path);
    if (fileSelection instanceof ChunkSelectState) {
      return fileSelection;
    }

    const cache = UseUncommittedSelection.fileContentCache;
    const maybeReadFromCache = (): ChunkSelectState | null => {
      const file = cache.files.get(path);
      if (file === undefined) {
        return null;
      }
      const parentPath = file?.copyFrom ?? path;
      const parentFile = cache.parentFiles.get(parentPath);
      if (parentFile?.dataBase85 || file?.dataBase85) {
        throw new Error(t('Cannot edit non-utf8 file'));
      }
      const a = parentFile?.data ?? '';
      const b = file?.data ?? '';
      const selected = this.getSelection(path);
      if (selected instanceof ChunkSelectState) {
        return selected;
      }
      const newSelection = this.selection.startChunkSelect(path, a, b, selected);
      this.setSelection(newSelection);
      const newSelected = newSelection.getSelection(path);
      assert(
        newSelected instanceof ChunkSelectState,
        'startChunkSelect() should provide ChunkSelectState',
      );
      return newSelected;
    };

    return cache.asyncLoadingLock.enqueueRun(async () => {
      const chunkState = maybeReadFromCache();
      if (chunkState !== null) {
        return chunkState;
      }

      // Not found in cache. Need to (re)load the file via the server.

      const revs = 'wdir()';
      // Setup event listener before sending the request.
      const iter = clientToServerAPI.iterateMessageOfType('exportedStack');
      // Explicitly ask for the file via assumeTracked. Note this also provides contents
      // of other tracked files.
      clientToServerAPI.postMessage({type: 'exportStack', revs, assumeTracked: [path]});
      for await (const event of iter) {
        if (event.revs !== revs) {
          // Ignore unrelated response.
          continue;
        }
        if (event.error) {
          throw new Error(event.error);
        }
        if (event.stack.some(c => !c.requested && c.node !== cache.wdirHash)) {
          // The wdirHash has changed. Fail the load.
          // The exported stack usually has a non-requested commit that is the parent of
          // the requested "wdir()", which is the "." commit that should match `wdirHash`.
          // Note: for an empty repo there is no such non-requested commit exported so
          // we skip the check in that case.
          throw new Error(t('Working copy has changed'));
        }

        // Update cache.
        event.stack.forEach(commit => {
          if (commit.requested) {
            mergeObjectToMap(commit.files, cache.files);
          } else {
            mergeObjectToMap(commit.relevantFiles, cache.parentFiles);
          }
        });

        // Try read from cache again.
        const chunkState = maybeReadFromCache();
        if (chunkState === null) {
          if (event.assumeTracked.includes(path)) {
            // We explicitly requested the file, but the server does not provide
            // it somehow.
            break;
          } else {
            // It's possible that there are multiple export requests.
            // This one does not provide the file we want, continue checking other responses.
            continue;
          }
        } else {
          return chunkState;
        }
      }

      // Handles the `break` above. Tells tsc that we don't return undefined.
      throw new Error(t('Unable to get file content unexpectedly'));
    });
  }

  /** Edit chunk selection for a file. */
  editChunkSelect(
    path: RepoRelativePath,
    newValue: ((chunkState: ChunkSelectState) => ChunkSelectState) | ChunkSelectState,
  ) {
    const newSelection = this.selection.editChunkSelect(path, newValue);
    this.setSelection(newSelection);
  }

  /**
   * Return true if a file is selected (default), false if deselected,
   * or a string with the edited content.
   */
  getSelection(path: RepoRelativePath): SingleFileSelection {
    return this.selection.getSelection(path);
  }

  isFullyOrPartiallySelected(path: RepoRelativePath): boolean {
    return this.selection.isFullyOrPartiallySelected(path);
  }

  isPartiallySelected(path: RepoRelativePath): boolean {
    return this.selection.isPartiallySelected(path);
  }

  isFullySelected(path: RepoRelativePath): boolean {
    return this.selection.isFullySelected(path);
  }

  isDeselected(path: RepoRelativePath): boolean {
    return this.selection.isDeselected(path);
  }

  isEverythingSelected(): boolean {
    return this.selection.isEverythingSelected(this.getPaths);
  }

  isNothingSelected(): boolean {
    return this.selection.isNothingSelected(this.getPaths);
  }

  hasChunkSelection(): boolean {
    return this.selection.hasChunkSelection();
  }
}

/** Get the uncommitted selection state. */
export function useUncommittedSelection() {
  const [selection, setSelection] = useRecoilState(uncommittedSelection);
  const uncommittedChanges = useRecoilValue(uncommittedChangesWithPreviews);
  const tree = useRecoilValue(treeWithPreviews);
  const wdirHash = tree.headCommit?.hash ?? '';
  const getPaths = () => uncommittedChanges.map(c => c.path);

  return new UseUncommittedSelection(selection, setSelection, wdirHash, getPaths);
}

function mergeObjectToMap<V>(obj: {[path: string]: V} | undefined, map: Map<string, V>) {
  if (obj === undefined) {
    return;
  }
  for (const k in obj) {
    const v = obj[k];
    map.set(k, v);
  }
}
