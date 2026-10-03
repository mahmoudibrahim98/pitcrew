// A new project's key, for creating projects from the scan's suggestions (`POST /v1/projects`
// needs one). A `ProjectKey` is 2 to 10 characters: an upper-case ASCII letter, then upper-case
// letters or digits (api-v1.md, "Projects and workstreams"). The rule:
//
// 1. The name's words: accents dropped (NFKD), upper-cased, split on anything that is not an ASCII
//    letter or digit. Digits before the first letter are dropped, since a key starts with a letter.
// 2. Two words or more give their initials, at most four ("Diffusion study" → `DS`,
//    "lab-tools" → `LT`). One word gives its first three characters ("paper" → `PAP`), and a
//    one-letter word is padded with `X`. A name with no ASCII letter at all gives `PRJ`.
// 3. Unique in the workspace: a key already taken (by a project, or by one created earlier in the
//    same batch) gets the smallest number from 2 that frees it (`PAP2`, `PAP3`, …).
//
// The hub still has the last word: two clients creating at once can both pick a free key, and the
// second gets `409`, so `hub-api.ts` takes it as taken and tries the next.

/** `ProjectKey`'s rule. */
export const PROJECT_KEY = /^[A-Z][A-Z0-9]{1,9}$/;

const MAX_LENGTH = 10;
const MAX_INITIALS = 4;
const SINGLE_WORD = 3;
const FALLBACK = 'PRJ';

/** The name's words, as rule 1 says. */
export function keyWords(name: string): string[] {
  const words = name
    .normalize('NFKD')
    .replace(/\p{M}+/gu, '')
    .toUpperCase()
    .split(/[^A-Z0-9]+/)
    .filter((word) => word !== '');
  while (words.length > 0) {
    const first = (words[0] ?? '').replace(/^[0-9]+/, '');
    if (first !== '') {
      words[0] = first;
      break;
    }
    words.shift();
  }
  return words;
}

/** The key a name suggests before it is made unique (rules 1 and 2). */
export function baseKey(name: string): string {
  const words = keyWords(name);
  const [first] = words;
  if (first === undefined) return FALLBACK;
  const key =
    words.length === 1
      ? first.slice(0, SINGLE_WORD)
      : words
          .slice(0, MAX_INITIALS)
          .map((word) => word.charAt(0))
          .join('');
  return key.length < 2 ? `${key}X` : key;
}

/** A key for a project called `name` that no key in `taken` has (rule 3). */
export function projectKeyFor(name: string, taken: ReadonlySet<string>): string {
  const base = baseKey(name);
  if (!taken.has(base)) return base;
  // Among `taken.size + 1` different candidates, one is free.
  for (let n = 2; n <= taken.size + 2; n += 1) {
    const suffix = String(n);
    const key = base.slice(0, MAX_LENGTH - suffix.length) + suffix;
    if (!taken.has(key)) return key;
  }
  throw new Error('No free project key');
}
