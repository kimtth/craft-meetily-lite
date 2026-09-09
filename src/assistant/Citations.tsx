import { Fragment } from 'react';
import type { TranscriptSegment } from '../types';

export type CitationPart = { kind: 'text'; text: string } | { kind: 'sources'; ids: string[] } | { kind: 'unresolved' };
export type CitationProps = {
  text: string;
  sourceIds: string[];
  sources: ReadonlyMap<string, TranscriptSegment | null>;
  disabled: boolean;
  onSeek: (seconds: number) => void;
};

// v1 positions address ONLY the saved text field's sourceIds array. Never
// interpret an eN alias as an array index (even if a transcript ID is eN).
// This is a display projection: neither text nor saved source lists are edited.
export function citationParts(text: string, sourceIds: readonly string[]): CitationPart[] {
  const parts: CitationPart[] = [];
  const groups = /\[\[cite:[^\r\n]*?\]\]|\[[^\[\]［］【】]*\]|［[^\[\]［］【】]*］|【[^\[\]［］【】]*】/gu;
  let cursor = 0;
  for (const match of text.matchAll(groups)) {
    const raw = match[0];
    const canonical = /^\[\[cite:v1:([1-9]\d*(?:,[1-9]\d*)*)\]\]$/.exec(raw);
    const legacy = /(?:^|[^\p{L}\p{N}_])[eEｅＥ][0-9０-９]/u.test(raw);
    if (!raw.startsWith('[[cite:') && !legacy) continue;
    if (match.index > cursor) parts.push({ kind: 'text', text: text.slice(cursor, match.index) });
    const positions = canonical?.[1].split(',').map(Number) ?? [];
    if (positions.length > 0 && positions.length <= 100 && positions.every(position =>
      Number.isSafeInteger(position) && position <= sourceIds.length && !!sourceIds[position - 1])) {
      parts.push({ kind: 'sources', ids: [...new Set(positions.map(position => sourceIds[position - 1]))] });
    } else {
      parts.push({ kind: 'unresolved' });
    }
    cursor = match.index + raw.length;
  }
  if (cursor < text.length) parts.push({ kind: 'text', text: text.slice(cursor) });
  return parts;
}

export function citationTimestamp(offset: number): string {
  const seconds = Math.floor(offset);
  const hours = Math.floor(seconds / 3600);
  return `${hours ? `${hours}:` : ''}${String(Math.floor(seconds % 3600 / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
}

export function citationSegment(id: string, sources: CitationProps['sources']): TranscriptSegment | null {
  const segment = sources.get(id);
  return segment && Number.isFinite(segment.offsetSeconds) && segment.offsetSeconds >= 0 ? segment : null;
}

export function sortedCitationIds(ids: readonly string[], sources: CitationProps['sources']): string[] {
  return [...new Set(ids)].sort((a, b) => {
    const left = citationSegment(a, sources)?.offsetSeconds ?? Infinity;
    const right = citationSegment(b, sources)?.offsetSeconds ?? Infinity;
    return left === right ? a.localeCompare(b) : left < right ? -1 : 1;
  });
}

export function CitationReference({ id, sources, disabled, onSeek }: Pick<CitationProps, 'sources' | 'disabled' | 'onSeek'> & { id: string }) {
  const segment = citationSegment(id, sources);
  if (!segment) return <span className="ma-unresolved">Reference unavailable</span>;
  const time = citationTimestamp(segment.offsetSeconds);
  return <span className="ma-source" tabIndex={disabled ? 0 : undefined} title={segment.text}
    aria-label={disabled ? `${time}: ${segment.text}. Playback unavailable.` : undefined}>
    <button type="button" className="ma-reference" disabled={disabled}
      aria-label={`Seek to ${time}: ${segment.text}`} onClick={() => { if (!disabled) onSeek(segment.offsetSeconds); }}>{time}</button>
    <span className="ma-source-preview" role="tooltip">{segment.text}{disabled && <span> · Playback unavailable</span>}</span>
  </span>;
}

export function CitationText({ text, sourceIds, ...props }: CitationProps) {
  return <>{citationParts(text, sourceIds).map((part, index) => <Fragment key={index}>
    {part.kind === 'text' ? part.text : part.kind === 'unresolved'
      ? <span className="ma-unresolved" title="This saved reference has no validated claim mapping. Sources below do not establish which source supports this claim.">[Reference unavailable]</span>
      : <span className="ma-inline-references" aria-label="Claim references">{sortedCitationIds(part.ids, props.sources).map(id =>
        <CitationReference key={id} id={id} {...props} />)}</span>}
  </Fragment>)}</>;
}

export function CitationSources({ text, sourceIds, ...props }: CitationProps) {
  const ids = sortedCitationIds(sourceIds, props.sources);
  if (!ids.length) return null;
  const mapped = citationParts(text, sourceIds).some(part => part.kind === 'sources');
  return <details className="ma-source-list">
    <summary>Sources ({ids.length})</summary>
    {!mapped && <p className="ma-muted">Claim-level reference mapping unavailable. These are sources for the saved response only.</p>}
    <ul className="ma-references" aria-label="Transcript references">{ids.map(id => <li key={id}>
      <CitationReference id={id} {...props} />
    </li>)}</ul>
  </details>;
}