// Errors and request validation.
//
// Handlers throw `ApiFailure`; the server turns it into an `ApiError` body with the status the
// contract gives its code. Request bodies are read through `Fields`, which checks each field's type
// and enum values and never copies unknown fields into state (like serde, unknown fields are
// ignored).

import { isWellFormedDate } from './rules.ts';
import type { ApiError, ErrorCode } from './types.ts';
import { isUlid } from './ulid.ts';

const STATUS_BY_CODE: Record<ErrorCode, number> = {
  unauthorized: 401,
  forbidden: 403,
  not_found: 404,
  conflict: 409,
  invalid: 400,
  unavailable: 503,
  internal: 500,
};

/** A failure with an `ApiError` code; the HTTP status follows from the code. */
export class ApiFailure extends Error {
  readonly code: ErrorCode;
  readonly status: number;

  constructor(code: ErrorCode, message: string) {
    super(message);
    this.code = code;
    this.status = STATUS_BY_CODE[code];
  }

  toBody(): ApiError {
    return { code: this.code, message: this.message };
  }
}

export const invalid = (message: string): ApiFailure => new ApiFailure('invalid', message);
export const notFound = (message: string): ApiFailure => new ApiFailure('not_found', message);
export const forbidden = (message: string): ApiFailure => new ApiFailure('forbidden', message);
export const conflict = (message: string): ApiFailure => new ApiFailure('conflict', message);
export const unavailable = (message: string): ApiFailure => new ApiFailure('unavailable', message);

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Checks `value` against an enum's value list. */
export function oneOf<T extends string>(value: unknown, values: readonly T[], what: string): T {
  const found = values.find((v) => v === value);
  if (found === undefined) {
    throw invalid(`${what} must be one of: ${values.join(', ')}`);
  }
  return found;
}

/**
 * Typed access to one JSON object from a request. `null` counts as absent, as it does for a Rust
 * `Option`. Every getter throws a 400 naming the field.
 */
export class Fields {
  readonly #object: Record<string, unknown>;
  readonly #where: string;

  constructor(value: unknown, where = 'body') {
    if (!isRecord(value)) {
      throw invalid(`${where} must be a JSON object`);
    }
    this.#object = value;
    this.#where = where;
  }

  /** Whether the key is present at all, even as `null`. */
  has(key: string): boolean {
    return Object.hasOwn(this.#object, key);
  }

  /** The raw value, for nested objects; `undefined` when absent or `null`. */
  raw(key: string): unknown {
    const value = this.has(key) ? this.#object[key] : undefined;
    return value === null ? undefined : value;
  }

  name(key: string): string {
    return this.#where === 'body' ? key : `${this.#where}.${key}`;
  }

  string(key: string): string {
    return this.#required(key, this.optString(key));
  }

  optString(key: string): string | undefined {
    const value = this.raw(key);
    if (value !== undefined && typeof value !== 'string') {
      throw invalid(`${this.name(key)} must be a string`);
    }
    return value;
  }

  /** A string with something other than whitespace in it. */
  text(key: string): string {
    const value = this.string(key);
    if (value.trim() === '') {
      throw invalid(`${this.name(key)} must not be empty`);
    }
    return value;
  }

  optText(key: string): string | undefined {
    return this.has(key) && this.raw(key) !== undefined ? this.text(key) : undefined;
  }

  bool(key: string): boolean {
    const value = this.raw(key);
    if (typeof value !== 'boolean') {
      throw invalid(`${this.name(key)} must be true or false`);
    }
    return value;
  }

  /** A whole number of at least `min`. */
  optInt(key: string, min = 0): number | undefined {
    const value = this.raw(key);
    if (value === undefined) {
      return undefined;
    }
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < min) {
      throw invalid(`${this.name(key)} must be a whole number of at least ${min}`);
    }
    return value;
  }

  int(key: string, min = 0): number {
    return this.#required(key, this.optInt(key, min));
  }

  enumOf<T extends string>(key: string, values: readonly T[]): T {
    return this.#required(key, this.optEnum(key, values));
  }

  optEnum<T extends string>(key: string, values: readonly T[]): T | undefined {
    const value = this.raw(key);
    return value === undefined ? undefined : oneOf(value, values, this.name(key));
  }

  optStringArray(key: string): string[] | undefined {
    const value = this.raw(key);
    if (value === undefined) {
      return undefined;
    }
    if (!Array.isArray(value) || !value.every((v): v is string => typeof v === 'string')) {
      throw invalid(`${this.name(key)} must be an array of strings`);
    }
    return [...value];
  }

  optArray(key: string): unknown[] | undefined {
    const value = this.raw(key);
    if (value !== undefined && !Array.isArray(value)) {
      throw invalid(`${this.name(key)} must be an array`);
    }
    return value;
  }

  /** A new id supplied by the client, such as a subtask id: a bare ULID, stored upper case. */
  ulid(key: string): string {
    const value = this.string(key);
    if (!isUlid(value)) {
      throw invalid(`${this.name(key)} must be a 26-character ULID`);
    }
    return value.toUpperCase();
  }

  optDate(key: string): string | undefined {
    const value = this.optString(key);
    if (value !== undefined && !isWellFormedDate(value)) {
      throw invalid(`${this.name(key)} must be a date written YYYY-MM-DD`);
    }
    return value;
  }

  #required<T>(key: string, value: T | undefined): T {
    if (value === undefined) {
      throw invalid(`${this.name(key)} is required`);
    }
    return value;
  }
}

/** A query parameter, treating an empty value as absent. */
export function queryValue(query: URLSearchParams, key: string): string | undefined {
  const value = query.get(key);
  return value === null || value === '' ? undefined : value;
}

/** Every non-empty value of a repeatable query parameter, checked against an enum. */
export function queryEnums<T extends string>(
  query: URLSearchParams,
  key: string,
  values: readonly T[],
): T[] {
  return query
    .getAll(key)
    .filter((v) => v !== '')
    .map((v) => oneOf(v, values, key));
}

/** A whole-number query parameter of at least `min`; `undefined` when absent. */
export function queryInt(query: URLSearchParams, key: string, min: number): number | undefined {
  const raw = queryValue(query, key);
  if (raw === undefined) {
    return undefined;
  }
  const value = /^\d{1,15}$/.test(raw) ? Number(raw) : Number.NaN;
  if (!(value >= min)) {
    throw invalid(`${key} must be a whole number of at least ${min}`);
  }
  return value;
}

/** A `limit` query parameter: at least 1, `fallback` when absent, and capped at `max`. */
export function queryLimit(query: URLSearchParams, fallback: number, max: number): number {
  return Math.min(queryInt(query, 'limit', 1) ?? fallback, max);
}
