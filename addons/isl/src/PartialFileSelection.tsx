/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

import type {RangeInfo} from './TextEditable';
import type {ChunkSelectState, LineRegion, SelectLine} from './stackEdit/chunkSelectState';

import {TextEditable} from './TextEditable';
import {T, t} from './i18n';
import {VSCodeCheckbox, VSCodeRadio} from '@vscode/webview-ui-toolkit/react';
import {Set as ImSet} from 'immutable';
import {useRef, useState} from 'react';
import {notEmpty} from 'shared/utils';

import './PartialFileSelection.css';

type Props = {
  chunkSelection: ChunkSelectState;
  setChunkSelection: (state: ChunkSelectState) => void;
};

export type PartialFileEditMode = 'unified' | 'side-by-side' | 'free-edit';

export function PartialFileSelection(props: Props) {
  const [editMode, setEditMode] = useState<PartialFileEditMode>('unified');

  return (
    <div>
      {/* Cannot use VSCodeRadioGroup. See https://github.com/microsoft/vscode-webview-ui-toolkit/issues/404 */}
      {/* FIXME: VSCodeRadio onClick does not fire on keyboard events (ex. tab, then space) */}
      <div>
        <VSCodeRadio checked={editMode === 'unified'} onClick={() => setEditMode('unified')}>
          <T>Unified</T>
        </VSCodeRadio>
        <VSCodeRadio
          checked={editMode === 'side-by-side'}
          onClick={() => setEditMode('side-by-side')}>
          <T>Side-by-side</T>
        </VSCodeRadio>
        <VSCodeRadio checked={editMode === 'free-edit'} onClick={() => setEditMode('free-edit')}>
          <T>Freeform edit</T>
        </VSCodeRadio>
      </div>
      <PartialFileSelectionWithMode {...props} mode={editMode} />
    </div>
  );
}

export function PartialFileSelectionWithMode(props: Props & {mode: PartialFileEditMode}) {
  if (props.mode === 'unified') {
    return <PartialFileSelectionWithCheckbox {...props} unified={true} />;
  } else if (props.mode === 'side-by-side') {
    return <PartialFileSelectionWithCheckbox {...props} unified={false} />;
  } else {
    return <PartialFileSelectionWithFreeEdit {...props} />;
  }
}

/** Show chunks with selection checkboxes. Supports unified and side-by-side modes. */
function PartialFileSelectionWithCheckbox(props: Props & {unified?: boolean}) {
  const unified = props.unified ?? true;

  // State for dragging on line numbers for range selection.
  const lastLine = useRef<SelectLine | null>(null);

  // Toggle selection of a line or a region.
  const toogleLineOrRegion = (line: SelectLine, region: LineRegion | null) => {
    const selected = !line.selected;
    const lineSelects: Array<[number, boolean]> = [];
    if (region) {
      region.lines.forEach(line => {
        lineSelects.push([line.rawIndex, selected]);
      });
    } else {
      lineSelects.push([line.rawIndex, selected]);
    }
    const newSelection = props.chunkSelection.setSelectedLines(lineSelects);
    lastLine.current = line;
    props.setChunkSelection(newSelection);
  };

  const handlePointerDown = (
    line: SelectLine,
    region: LineRegion | null,
    e: React.PointerEvent,
  ) => {
    if (e.isPrimary && line.selected !== null) {
      toogleLineOrRegion(line, region);
    }
  };

  // Toogle selection of a single line.
  const handlePointerEnter = (line: SelectLine, e: React.PointerEvent<HTMLDivElement>) => {
    if (e.buttons === 1 && line.selected !== null && lastLine.current?.rawIndex !== line.rawIndex) {
      const newSelection = props.chunkSelection.setSelectedLines([[line.rawIndex, !line.selected]]);
      lastLine.current = line;
      props.setChunkSelection(newSelection);
    }
  };

  const lineCheckbox: JSX.Element[] = [];
  const lineANumber: JSX.Element[] = [];
  const lineBNumber: JSX.Element[] = [];
  const lineAContent: JSX.Element[] = []; // side by side left, or unified
  const lineBContent: JSX.Element[] = unified ? lineAContent : []; // side by side right

  const lineRegions = props.chunkSelection.getLineRegions();
  lineRegions.forEach((region, regionIndex) => {
    const key = region.lines[0].rawIndex;
    if (region.collapsed) {
      // Skip "~~~~" for the first and last collapsed region.
      if (regionIndex > 0 && regionIndex + 1 < lineRegions.length) {
        const contextLineContent = <div key={key} className="line line-context" />;
        lineAContent.push(contextLineContent);
        if (!unified) {
          lineBContent.push(contextLineContent);
        }
        const contextLineNumber = <div key={key} className="line-number line-context" />;
        lineCheckbox.push(contextLineNumber);
        lineANumber.push(contextLineNumber);
        lineBNumber.push(contextLineNumber);
      }
      return;
    }

    if (!region.same) {
      const selectableCount = region.lines.reduce(
        (acc, line) => acc + (line.selected != null ? 1 : 0),
        0,
      );
      if (selectableCount > 0) {
        const selectedCount = region.lines.reduce((acc, line) => acc + (line.selected ? 1 : 0), 0);
        const indeterminate = selectedCount > 0 && selectedCount < selectableCount;
        const checked = selectedCount === selectableCount;
        // Note: VSCodeCheckbox's onClick or onChange are not really React events
        // and are hard to get right (ex. onChange can be triggered by re-rendering
        // with a different `checked` state without events on the checkbox itself).
        // So we use onClick on the parent element.
        // FIXME: This does not work for keyboard checkbox events.
        lineCheckbox.push(
          <div className="checkbox-anchor">
            <div
              key={`${key}c`}
              className="checkbox-container"
              onClick={_e => toogleLineOrRegion(region.lines[0], region)}>
              <VSCodeCheckbox checked={checked} indeterminate={indeterminate} />
            </div>
          </div>,
        );
      }
    }

    let regionALineCount = 0;
    let regionBLineCount = 0;
    region.lines.forEach(line => {
      const lineClasses = ['line'];
      const isAdd = line.sign.includes('+');
      if (isAdd) {
        lineClasses.push('line-add');
      } else if (line.sign.includes('-')) {
        lineClasses.push('line-del');
      }

      const lineNumberClasses = ['line-number'];
      if (line.selected != null) {
        lineNumberClasses.push('selectable');
      }
      if (line.selected) {
        lineNumberClasses.push('selected');
      }

      const hasA = unified || line.aLine != null;
      const hasB =
        unified ||
        line.bLine != null ||
        isAdd; /* isAdd is for "line.bits == 0b010", added by manual editing */
      const key = line.rawIndex;
      const handlerProps = {
        onPointerDown: handlePointerDown.bind(null, line, null),
        onPointerEnter: handlePointerEnter.bind(null, line),
      };

      if (hasA) {
        lineANumber.push(
          <div key={key} className={lineNumberClasses.join(' ')} {...handlerProps}>
            {line.aLine}
            {'\n'}
          </div>,
        );
        lineAContent.push(
          <div key={key} className={lineClasses.join(' ')}>
            {line.data}
          </div>,
        );
        regionALineCount += 1;
      }
      if (hasB) {
        lineBNumber.push(
          <div key={key} className={lineNumberClasses.join(' ')} {...handlerProps}>
            {line.bLine}
            {'\n'}
          </div>,
        );
        if (!unified) {
          lineBContent.push(
            <div key={key} className={lineClasses.join(' ')}>
              {line.data}
            </div>,
          );
          regionBLineCount += 1;
        }
      }
    });

    if (!unified) {
      let columns: JSX.Element[][] = [];
      let count = 0;
      if (regionALineCount < regionBLineCount) {
        columns = [lineANumber, lineAContent];
        count = regionBLineCount - regionALineCount;
      } else if (regionALineCount > regionBLineCount) {
        columns = [lineBNumber, lineBContent];
        count = regionALineCount - regionBLineCount;
      }
      for (let i = 0; i < count; i++) {
        columns.forEach(column => column.push(<div key={`${key}-pad-${i}`}>{'\n'}</div>));
      }
    }

    for (let i = 0; i < Math.max(regionALineCount, regionBLineCount); i++) {
      lineCheckbox.push(<div key={`${key}-pad-${i}`}>{'\n'}</div>);
    }
  });

  return (
    <div className="partial-file-selection-width-min-content partial-file-selection-border">
      <div className="partial-file-selection-scroll-y">
        <div className="partial-file-selection checkboxes">
          <pre className="column-checkbox">{lineCheckbox}</pre>
          {unified ? (
            <>
              <pre className="column-a-number">{lineANumber}</pre>
              <pre className="column-b-number">{lineBNumber}</pre>
              <div className="partial-file-selection-scroll-x">
                <pre className="column-unified">{lineAContent}</pre>
              </div>
            </>
          ) : (
            <>
              <pre className="column-a-number">{lineANumber}</pre>
              <div className="partial-file-selection-scroll-x">
                <pre className="column-a">{lineAContent}</pre>
              </div>
              <pre className="column-b-number">{lineBNumber}</pre>
              <div className="partial-file-selection-scroll-x">
                <pre className="column-b">{lineBContent}</pre>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

/** Show 3 editors side-by-side: `|A|M|B|`. `M` allows editing. No checkboxes. */
function PartialFileSelectionWithFreeEdit(props: Props) {
  // States for context line expansion.
  const [expandedALines, setExpandedALines] = useState<ImSet<number>>(ImSet);
  const [currentCaretLine, setCurrentSelLine] = useState<number>(-1);

  const lineRegions = props.chunkSelection.getLineRegions({
    expandedALines,
    expandedSelLine: currentCaretLine,
  });

  // Needed by TextEditable. Ranges of text on the right side.
  const rangeInfos: RangeInfo[] = [];
  let start = 0;

  // Render the rows.
  // We draw 3 editors: A (working parent), M (selected), B (working copy).
  // A and B are read-only. M is editable. The user selects content from
  // either A or B to change content of M.
  const lineAContent: JSX.Element[] = [];
  const lineBContent: JSX.Element[] = [];
  const lineMContent: JSX.Element[] = [];
  const lineANumber: JSX.Element[] = [];
  const lineBNumber: JSX.Element[] = [];
  const lineMNumber: JSX.Element[] = [];

  const insertContextLines = (lines: Readonly<SelectLine[]>) => {
    const handleExpand = () => {
      // Only the "unchanged" lines need expansion.
      // We use line numbers on the "a" side, which remains "stable" regardless of editing.
      const newLines = lines.map(l => l.aLine).filter(notEmpty);
      const newSet = expandedALines.union(newLines);
      setExpandedALines(newSet);
    };
    const key = lines[0].rawIndex;
    const contextLineContent = (
      <div
        key={key}
        className="line line-context"
        title={t('Click to expand lines.')}
        onClick={handleExpand}
      />
    );
    lineAContent.push(contextLineContent);
    lineBContent.push(contextLineContent);
    lineMContent.push(contextLineContent);
    const contextLineNumber = <div key={key} className="line-number line-context" />;
    lineANumber.push(contextLineNumber);
    lineBNumber.push(contextLineNumber);
    lineMNumber.push(contextLineNumber);
  };

  lineRegions.forEach(region => {
    if (region.collapsed) {
      // Draw "~~~" between chunks.
      insertContextLines(region.lines);
    }

    const regionClass = region.same ? 'region-same' : 'region-diff';

    let regionALineCount = 0;
    let regionBLineCount = 0;
    let regionMLineCount = 0;
    region.lines.forEach(line => {
      let dataRangeId = undefined;
      // Provide `RangeInfo` for editing, if the line exists in the selection version.
      // This is also needed for "~~~" context lines.
      if (line.selLine !== null) {
        const end = start + line.data.length;
        dataRangeId = rangeInfos.length;
        rangeInfos.push({start, end});
        start = end;
      }

      if (region.collapsed) {
        return;
      }

      // Draw the actual line and line numbers.
      let lineAClass = 'line line-a';
      let lineBClass = 'line line-b';
      let lineMClass = 'line line-m';

      // Find the "unique" lines (different with other versions). They will be highlighted.
      switch (line.bits) {
        case 0b100:
          lineAClass += ' line-unique';
          break;
        case 0b010:
          lineMClass += ' line-unique';
          break;
        case 0b001:
          lineBClass += ' line-unique';
          break;
      }

      const key = line.rawIndex;
      if (line.aLine !== null) {
        lineAContent.push(
          <div key={key} className={`${lineAClass} ${regionClass}`}>
            {line.data}
          </div>,
        );
        lineANumber.push(
          <div key={key} className={`line-number line-a ${regionClass}`}>
            {line.aLine}
          </div>,
        );
        regionALineCount += 1;
      }
      if (line.bLine !== null) {
        lineBContent.push(
          <div key={key} className={`${lineBClass} ${regionClass}`}>
            {line.data}
          </div>,
        );
        lineBNumber.push(
          <div key={key} className={`line-number line-b ${regionClass}`}>
            {line.bLine}
          </div>,
        );
        regionBLineCount += 1;
      }
      if (line.selLine !== null) {
        lineMContent.push(
          <div key={key} className={`${lineMClass} ${regionClass}`} data-range-id={dataRangeId}>
            {line.data}
          </div>,
        );
        lineMNumber.push(
          <div key={key} className={`line-number line-m ${regionClass}`}>
            {line.selLine}
          </div>,
        );
        regionMLineCount += 1;
      }
    });

    // Add padding lines to align the "bottom" of the region.
    const regionPadLineCount = Math.max(regionALineCount, regionBLineCount, regionMLineCount);
    const key = region.lines[0].rawIndex;
    (
      [
        [lineAContent, lineANumber, regionALineCount],
        [lineBContent, lineBNumber, regionBLineCount],
        [lineMContent, lineMNumber, regionMLineCount],
      ] as [JSX.Element[], JSX.Element[], number][]
    ).forEach(([lineContent, lineNumber, lineCount]) => {
      for (let i = 0; i < regionPadLineCount - lineCount; i++) {
        lineContent.push(
          <div key={`${key}-pad-${i}`} className="line">
            {'\n'}
          </div>,
        );
        lineNumber.push(
          <div key={`${key}-pad-${i}`} className="line-number">
            {'\n'}
          </div>,
        );
      }
    });
  });

  const textValue = props.chunkSelection.getSelectedText();
  const handleTextChange = (text: string) => {
    const newChunkSelect = props.chunkSelection.setSelectedText(text);
    props.setChunkSelection(newChunkSelect);
  };
  const handleSelChange = (start: number, end: number) => {
    // Expand the line of the cursor. But do not expand on range selection (ex. Ctrl+A).
    if (start === end) {
      let selLine = countLines(textValue.substring(0, start));
      if (start == textValue.length && textValue.endsWith('\n')) {
        selLine -= 1;
      }
      setCurrentSelLine(selLine);
    }
  };

  return (
    <div className="partial-file-selection-width-min-content partial-file-selection-border">
      <div className="partial-file-selection-scroll-y">
        <div className="partial-file-selection free-form">
          <pre className="column-a-number readonly">{lineANumber}</pre>
          <div className="partial-file-selection-scroll-x readonly">
            <pre className="column-a">{lineAContent}</pre>
          </div>
          <pre className="column-m-number">{lineMNumber}</pre>
          <div className="partial-file-selection-scroll-x">
            <pre className="column-m">
              <TextEditable
                value={textValue}
                rangeInfos={rangeInfos}
                onTextChange={handleTextChange}
                onSelectChange={handleSelChange}>
                <pre className="column-m">{lineMContent}</pre>
              </TextEditable>
            </pre>
          </div>
          <pre className="column-b-number readonly">{lineBNumber}</pre>
          <div className="partial-file-selection-scroll-x readonly">
            <pre className="column-b">{lineBContent}</pre>
          </div>
        </div>
      </div>
    </div>
  );
}

function countLines(text: string): number {
  let result = 1;
  for (const ch of text) {
    if (ch === '\n') {
      result++;
    }
  }
  return result;
}
