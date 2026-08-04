/**
 * One citation.
 *
 * Shows the project, the repository-relative path, the line range, and
 * the section — never an absolute path. Orbit's own source references
 * are project-relative by construction; rendering anything wider would
 * put a private directory layout on screen for no benefit.
 */

import { memo } from "react";
import type { SourceItem } from "../state/session";

interface Props {
  source: SourceItem;
  onSelect?: (source: SourceItem) => void;
}

export function formatRange(source: SourceItem): string | undefined {
  if (source.lineStart === undefined) return undefined;
  if (source.lineEnd === undefined || source.lineEnd === source.lineStart) {
    return `:${source.lineStart}`;
  }
  return `:${source.lineStart}-${source.lineEnd}`;
}

export const SourceCitation = memo(function SourceCitation({ source, onSelect }: Props) {
  const range = formatRange(source);
  return (
    <li>
      <button className="source" onClick={() => onSelect?.(source)}>
        {source.project && <span className="proj">{source.project}</span>}
        <span className="path">{source.path}</span>
        {range && <span className="lines">{range}</span>}
        {source.section && <span className="section">{source.section}</span>}
      </button>
    </li>
  );
});
