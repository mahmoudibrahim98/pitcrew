// Wire types the console needs that `src/data/types.ts` does not have yet, written by hand from
// `crates/protocol/src/transcript.rs` and `runner.rs`. They belong in `src/data` once stream L
// takes them (see this folder's README).

import type { TimestampMs } from '../data/index.ts';

export type PlanStatus = 'pending' | 'in_progress' | 'completed';

export interface PlanItem {
  text: string;
  status: PlanStatus;
}

/** `TranscriptItem`, tagged by `kind`. Every item carries the byte offset of its record. */
export type TranscriptItem =
  | { kind: 'user_prompt'; at: TimestampMs; text: string; offset: number }
  | { kind: 'assistant_text'; at: TimestampMs; text: string; offset: number }
  | {
      kind: 'tool_use';
      at: TimestampMs;
      call_id: string;
      tool: string;
      target: string;
      input?: unknown;
      offset: number;
    }
  | {
      kind: 'tool_result';
      at: TimestampMs;
      call_id: string;
      is_error: boolean;
      summary: string;
      offset: number;
    }
  | {
      kind: 'file_edit';
      at: TimestampMs;
      path: string;
      added: number;
      removed: number;
      diff?: string;
      offset: number;
    }
  | { kind: 'plan_updated'; at: TimestampMs; items: PlanItem[]; offset: number }
  | { kind: 'question'; at: TimestampMs; text: string; options: string[]; offset: number }
  | { kind: 'turn_ended'; at: TimestampMs; offset: number };

export type TranscriptKind = TranscriptItem['kind'];

export type ItemOf<K extends TranscriptKind> = Extract<TranscriptItem, { kind: K }>;

/** Every kind this build renders. The Rust enum is `#[non_exhaustive]`: newer kinds are skipped. */
export const TRANSCRIPT_KINDS = [
  'user_prompt',
  'assistant_text',
  'tool_use',
  'tool_result',
  'file_edit',
  'plan_updated',
  'question',
  'turn_ended',
] as const satisfies readonly TranscriptKind[];

/** `GET /v1/sessions/{id}/transcript`: items oldest first; `from` is the `before` of the previous page. */
export interface TranscriptPage {
  items: TranscriptItem[];
  from: number;
  to: number;
  at_start: boolean;
}

/** `runner.rs` `Key`, for `POST /v1/sessions/{id}/keys`. */
export type Key = 'enter' | 'escape' | 'tab' | 'up' | 'down' | 'left' | 'right' | 'backspace' | 'ctrl_c';

/** `runner.rs` `EndMode`. */
export type EndMode = 'graceful' | 'kill';
