// Remote workspaces and SSH's prompts, as the desktop gateway gives them
// (`docs/build/contracts/desktop-gateway.md`, "Remote workspaces" and "Prompts"). The types, the
// interface the UI calls, and the checks every incoming payload passes: the gateway's answers and
// events are read, never trusted, and a malformed one is dropped (an event) or refused (an answer).
//
// No Tauri here: `gateway.ts` implements `RemoteGateway`, and only it and `desktop.tsx` import
// the checks, so they stay out of the browser's first chunk.

import type { GatewayWorkspace, WorkspaceState } from './workspaces.tsx';

/** `gateway_remote_probe`: what is on the remote, found without changing anything. */
export interface RemoteProbe {
  host: string;
  /** e.g. "linux", "x86_64". */
  os: string;
  arch: string;
  /** PitCrew's helper (`pitcrewd`), if it is there. */
  helper?: { version: string; running: boolean } | undefined;
  slurm?: { version: string; defaultPartition?: string | undefined; srunOverlap: boolean } | undefined;
  /** tmux, if it is there: the tmux launcher needs 3.2 or newer. Absent means not known. */
  tmux?: { version: string } | undefined;
}

/** How the remote's helper runs. */
export type RemoteLauncher = 'direct' | 'tmux' | 'slurm';

/** SLURM job options; each left out takes the site's default. */
export interface JobOptions {
  partition?: string;
  account?: string;
  qos?: string;
  time?: string;
  cpus?: number;
  memory?: string;
  gpus?: string;
}

/** `gateway_remote_plan`'s request. */
export interface RemotePlanRequest {
  host: string;
  launcher: RemoteLauncher;
  /** A site recipe's name, for slurm. */
  site?: string;
  job?: JobOptions;
}

/** What adding would do, without doing it. */
export interface RemotePlan {
  /** An opaque id, valid for 10 minutes and used once. */
  plan: string;
  steps: string[];
  /** slurm: exactly the text that will be submitted. */
  jobScript?: string | undefined;
}

/**
 * One message on `gateway_remote_add`'s channel. `step` is one of the plan's `steps` (a `running`
 * message may come again with a `detail`: "40% sent", "job 4242 pending"); the last message is
 * `{ step: 'add', state: 'done' | 'failed', detail? }`, for the whole add. A SLURM add can take
 * minutes: there is no time limit here.
 */
export interface RemoteProgress {
  step: string;
  state: 'running' | 'done' | 'failed';
  detail?: string | undefined;
}

/**
 * `password`, `passphrase` and `otp` take an `answer`; `host_key` and `confirm` (ssh's other yes/no
 * questions) take `accept`; a `notice` ("touch your security key") takes nothing, and is closed when
 * ssh moves on.
 */
export type PromptKind = 'password' | 'passphrase' | 'otp' | 'host_key' | 'confirm' | 'notice';

export const PROMPT_KINDS: readonly PromptKind[] = ['password', 'passphrase', 'otp', 'host_key', 'confirm', 'notice'];

/** `gateway://prompt`: SSH asks something. `text` is untrusted: show it as text, never markup. */
export interface GatewayPrompt {
  id: string;
  host: string;
  kind: PromptKind;
  text: string;
  /** host_key: the key's fingerprint, to compare. */
  fingerprint?: string | undefined;
}

/** `answer` for a password, passphrase or code; `accept` for a host key or a confirm; neither cancels (and stops ssh). */
export type PromptReply = { answer: string } | { accept: boolean } | Record<string, never>;

/**
 * The gateway's remote commands and prompt events (desktop only: a browser has none). Every
 * rejection is a `GatewayError`; a refused or expired plan is `invalid`, a lost connection
 * `unreachable`. Any of these may cause a prompt, which `onPrompt` delivers.
 */
export interface RemoteGateway {
  /** The concrete `Host` names in the person's ssh config. The person may also type a host. */
  sshHosts(): Promise<string[]>;
  remoteProbe(host: string): Promise<RemoteProbe>;
  remotePlan(request: RemotePlanRequest): Promise<RemotePlan>;
  /** Carries out a plan: deploy, launch, pair, register. `onProgress` gets each checked message. */
  remoteAdd(plan: string, onProgress: (progress: RemoteProgress) => void): Promise<GatewayWorkspace>;
  /** Forgets a workspace and its token; with `stopHelper`, first stops the remote helper (and its SLURM job). */
  workspaceRemove(workspace: string, stopHelper: boolean): Promise<void>;
  /**
   * Tries an `unreachable` remote workspace's connection again at once (after a cancelled sign-in,
   * say). Resolves once the attempt has started; its state follows on `gateway://workspaces`.
   */
  workspaceRetry(workspace: string): Promise<void>;
  /** Follows `gateway://prompt`; malformed prompts are dropped. Resolves to an unsubscribe. */
  onPrompt(listener: (prompt: GatewayPrompt) => void): Promise<() => void>;
  /** Follows `gateway://prompt-closed`: the prompt `id` is no longer wanted. */
  onPromptClosed(listener: (id: string) => void): Promise<() => void>;
  /** Answers a prompt. The answer is passed on once and kept nowhere. */
  replyPrompt(id: string, reply: PromptReply): Promise<void>;
}

// ─── Checks ────────────────────────────────────────────────────────────────────────────────────

const STATES: readonly WorkspaceState[] = ['connecting', 'ready', 'unreachable', 'needs_pairing'];
const PROGRESS_STATES: readonly RemoteProgress['state'][] = ['running', 'done', 'failed'];

/** Longest text kept from a prompt or a progress detail; the rest is cut. */
const MAX_TEXT = 2_000;
const MAX_SHORT = 512;

// eslint-disable-next-line no-control-regex
const CONTROL_EXCEPT_LINES = /[\u0000-\u0008\u000B-\u001F\u007F-\u009F]/g;
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001F\u007F-\u009F]/g;

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

/** Text for people to read: control characters out (new lines and tabs kept), at most `max`. */
function cleanText(value: string, max = MAX_TEXT): string {
  return value.replace(CONTROL_EXCEPT_LINES, '').slice(0, max);
}

/** A one-line string: every control character out, at most `max`. */
function cleanLine(value: string, max = MAX_SHORT): string {
  return value.replace(CONTROL, '').slice(0, max);
}

function optionalString(value: unknown): string | undefined | false {
  if (value === undefined || value === null) return undefined;
  return typeof value === 'string' ? value : false;
}

export function isGatewayWorkspace(value: unknown): value is GatewayWorkspace {
  const v = record(value);
  if (v === undefined) return false;
  return (
    typeof v.id === 'string' &&
    v.id !== '' &&
    typeof v.name === 'string' &&
    (v.kind === 'local' || v.kind === 'remote') &&
    STATES.includes(v.state as WorkspaceState) &&
    (v.detail === undefined || v.detail === null || typeof v.detail === 'string')
  );
}

/** `{ hosts: string[] }`: the host names that are non-empty one-line strings; others are dropped. */
export function parseHosts(value: unknown): string[] {
  const hosts = record(value)?.hosts;
  if (!Array.isArray(hosts)) return [];
  const seen = new Set<string>();
  for (const host of hosts) {
    if (typeof host === 'string' && host !== '' && cleanLine(host) === host) seen.add(host);
  }
  return [...seen];
}

export function parseRemoteProbe(value: unknown): RemoteProbe | undefined {
  const v = record(value);
  if (v === undefined || typeof v.host !== 'string' || typeof v.os !== 'string' || typeof v.arch !== 'string') {
    return undefined;
  }
  const probe: RemoteProbe = { host: cleanLine(v.host), os: cleanLine(v.os), arch: cleanLine(v.arch) };
  if (v.helper !== undefined && v.helper !== null) {
    const helper = record(v.helper);
    if (helper === undefined || typeof helper.version !== 'string' || typeof helper.running !== 'boolean') return undefined;
    probe.helper = { version: cleanLine(helper.version), running: helper.running };
  }
  if (v.slurm !== undefined && v.slurm !== null) {
    const slurm = record(v.slurm);
    const partition = optionalString(slurm?.defaultPartition);
    if (slurm === undefined || typeof slurm.version !== 'string' || typeof slurm.srunOverlap !== 'boolean' || partition === false) {
      return undefined;
    }
    probe.slurm = {
      version: cleanLine(slurm.version),
      srunOverlap: slurm.srunOverlap,
      ...(partition === undefined ? {} : { defaultPartition: cleanLine(partition) }),
    };
  }
  if (v.tmux !== undefined && v.tmux !== null) {
    const tmux = record(v.tmux);
    if (tmux === undefined || typeof tmux.version !== 'string') return undefined;
    probe.tmux = { version: cleanLine(tmux.version) };
  }
  return probe;
}

/** A plan, with its job script kept verbatim: it is exactly what will be submitted. */
export function parseRemotePlan(value: unknown): RemotePlan | undefined {
  const v = record(value);
  if (v === undefined || typeof v.plan !== 'string' || v.plan === '' || !Array.isArray(v.steps)) return undefined;
  if (!v.steps.every((step) => typeof step === 'string')) return undefined;
  const jobScript = optionalString(v.jobScript);
  if (jobScript === false) return undefined;
  return {
    plan: v.plan,
    steps: (v.steps as string[]).map((step) => cleanText(step, MAX_SHORT)),
    ...(jobScript === undefined ? {} : { jobScript }),
  };
}

export function parseProgress(value: unknown): RemoteProgress | undefined {
  const v = record(value);
  if (v === undefined || typeof v.step !== 'string' || !PROGRESS_STATES.includes(v.state as RemoteProgress['state'])) {
    return undefined;
  }
  const detail = optionalString(v.detail);
  if (detail === false) return undefined;
  return {
    step: cleanLine(v.step),
    state: v.state as RemoteProgress['state'],
    ...(detail === undefined ? {} : { detail: cleanText(detail) }),
  };
}

export function parsePrompt(value: unknown): GatewayPrompt | undefined {
  const v = record(value);
  if (v === undefined || typeof v.id !== 'string' || v.id === '' || typeof v.host !== 'string' || v.host === '') {
    return undefined;
  }
  if (!PROMPT_KINDS.includes(v.kind as PromptKind) || typeof v.text !== 'string') return undefined;
  const fingerprint = optionalString(v.fingerprint);
  if (fingerprint === false) return undefined;
  return {
    id: v.id,
    host: cleanLine(v.host),
    kind: v.kind as PromptKind,
    text: cleanText(v.text),
    ...(fingerprint === undefined ? {} : { fingerprint: cleanLine(fingerprint) }),
  };
}

/** `gateway://prompt-closed`'s `{ id }`. */
export function parsePromptClosed(value: unknown): string | undefined {
  const id = record(value)?.id;
  return typeof id === 'string' && id !== '' ? id : undefined;
}
