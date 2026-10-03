// How a request fails. Features handle `ApiError`; the desktop gateway's failures are `ApiError`s
// too (`GatewayError`), so that handling applies to them unchanged.

import type { ErrorCode } from './types.ts';

/** A failed request. `status` is 0 when the hub could not be reached at all. */
export class ApiError extends Error {
  readonly code: ErrorCode;
  readonly status: number;
  readonly size: number | undefined;
  readonly current_revision: string | null | undefined;

  constructor(code: ErrorCode, message: string, status: number, details: { size?: number; current_revision?: string | null } = {}) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
    this.status = status;
    this.size = details.size;
    this.current_revision = details.current_revision;
  }
}

/** Why the desktop gateway could not forward a call (`docs/build/contracts/desktop-gateway.md`). */
export type GatewayErrorCode =
  | 'unknown_workspace'
  | 'needs_pairing'
  | 'unreachable'
  | 'invalid'
  | 'too_large'
  | 'internal';

const GATEWAY_CODES: readonly GatewayErrorCode[] = [
  'unknown_workspace',
  'needs_pairing',
  'unreachable',
  'invalid',
  'too_large',
  'internal',
];

/** The nearest API code, so code written for HTTP failures handles these too. */
const API_CODE: Record<GatewayErrorCode, ErrorCode> = {
  unknown_workspace: 'not_found',
  needs_pairing: 'unauthorized',
  // The same as a network failure.
  unreachable: 'unavailable',
  invalid: 'invalid',
  too_large: 'invalid',
  internal: 'internal',
};

/**
 * A call the gateway could not forward. The daemon never answered, so `status` is 0, as for an
 * unreachable hub. `code` is the nearest API code; `gateway` is the gateway's own, which tells
 * `needs_pairing` apart from a rejected token.
 */
export class GatewayError extends ApiError {
  readonly gateway: GatewayErrorCode;

  constructor(gateway: GatewayErrorCode, message: string) {
    super(API_CODE[gateway], message, 0);
    this.name = 'GatewayError';
    this.gateway = gateway;
  }
}

/** Reads a rejected gateway command: `{ code, message }`; anything else is `internal`. */
export function toGatewayError(reason: unknown): GatewayError {
  if (reason instanceof GatewayError) return reason;
  if (typeof reason === 'object' && reason !== null) {
    const { code, message } = reason as Record<string, unknown>;
    if (GATEWAY_CODES.includes(code as GatewayErrorCode)) {
      return new GatewayError(code as GatewayErrorCode, typeof message === 'string' ? message : String(code));
    }
  }
  if (typeof reason === 'string' && reason !== '') return new GatewayError('internal', reason);
  if (reason instanceof Error && reason.message !== '') return new GatewayError('internal', reason.message);
  return new GatewayError('internal', 'The desktop gateway failed.');
}
