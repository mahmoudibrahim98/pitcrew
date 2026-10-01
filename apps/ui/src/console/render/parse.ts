// Everything the render worker parses. The main thread imports this only when no worker runs.

import { parseDiff, type ParsedDiff } from './diff-parse.ts';
import { parseMarkdown, type Block } from './markdown-parse.ts';

export interface ParseResults {
  markdown: Block[];
  diff: ParsedDiff;
}

export type ParseKind = keyof ParseResults;

export interface ParseRequest {
  id: number;
  kind: ParseKind;
  text: string;
}

export type ParseResponse = { id: number; result: ParseResults[ParseKind] } | { id: number; error: string };

export function parseAny(kind: ParseKind, text: string): ParseResults[ParseKind] {
  return kind === 'markdown' ? parseMarkdown(text) : parseDiff(text);
}
