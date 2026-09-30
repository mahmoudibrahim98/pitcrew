// ULIDs: 48 bits of milliseconds, then 80 random bits, in Crockford base32 (26 characters).
//
// Ids made in the same millisecond reuse its random part plus one, so ids from this process always
// sort in creation order (the spec's "monotonic" mode).

import { randomBytes } from 'node:crypto';

const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
const RANDOM_MAX = (1n << 80n) - 1n;

let lastTime = -1;
let lastRandom = 0n;

/** A new ULID. */
export function ulid(): string {
  const now = Date.now();
  if (now > lastTime) {
    lastTime = now;
    lastRandom = BigInt(`0x${randomBytes(10).toString('hex')}`);
  } else if (lastRandom === RANDOM_MAX) {
    // The random part ran out within one millisecond: borrow the next millisecond.
    lastTime += 1;
    lastRandom = 0n;
  } else {
    lastRandom += 1n;
  }
  return encodeTime(lastTime) + encodeRandom(lastRandom);
}

/** Whether `text` is a well-formed ULID (either case). */
export function isUlid(text: string): boolean {
  return /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/i.test(text);
}

function encodeTime(time: number): string {
  let out = '';
  let rest = time;
  for (let i = 0; i < 10; i++) {
    out = ALPHABET.charAt(rest % 32) + out;
    rest = Math.floor(rest / 32);
  }
  return out;
}

function encodeRandom(random: bigint): string {
  let out = '';
  let rest = random;
  for (let i = 0; i < 16; i++) {
    out = ALPHABET.charAt(Number(rest & 31n)) + out;
    rest >>= 5n;
  }
  return out;
}
