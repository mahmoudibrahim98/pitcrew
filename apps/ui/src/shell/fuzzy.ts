// Fuzzy matching for the palette. An exact match beats a prefix, a prefix beats a word start, a
// word start beats a substring, and a substring beats letters scattered in order.

const BOUNDARY = /[\s\-_./·:#@]/;

function atBoundary(text: string, at: number): boolean {
  return at === 0 || BOUNDARY.test(text[at - 1] ?? ' ');
}

/** How well one query word matches `text`, from 0 to 100; null when it does not. Both lowercase. */
export function scoreWord(word: string, text: string): number | null {
  if (word === '') return 0;
  if (text === word) return 100;
  const at = text.indexOf(word);
  const coverage = (word.length / text.length) * 10;
  if (at === 0) return 80 + coverage;
  if (at > 0) {
    // Prefer a later word start over an earlier mid-word hit.
    let start = at;
    while (start !== -1 && !atBoundary(text, start)) start = text.indexOf(word, start + 1);
    if (start !== -1) return 60 + coverage - Math.min(start, 20) * 0.25;
    return 40 + coverage - Math.min(at, 20) * 0.25;
  }
  // Letters in order: consecutive letters and word starts count most.
  let total = 0;
  let from = 0;
  let previous = -2;
  for (const letter of word) {
    const found = text.indexOf(letter, from);
    if (found === -1) return null;
    total += found === previous + 1 ? 1 : atBoundary(text, found) ? 0.8 : 0.3;
    previous = found;
    from = found + 1;
  }
  return (total / word.length) * 35;
}

/**
 * Scores a query against an item's fields (key, title, …): every word of the query must match
 * some field; the score adds each word's best. The first field counts a little more. Null when
 * a word matches nothing.
 */
export function scoreFields(query: string, fields: readonly string[]): number | null {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return 0;
  const lowered = fields.map((f) => f.toLowerCase());
  let total = 0;
  for (const word of words) {
    let best = -1;
    for (const [i, field] of lowered.entries()) {
      const score = scoreWord(word, field);
      if (score !== null) best = Math.max(best, i === 0 ? score * 1.05 : score);
    }
    if (best < 0) return null;
    total += best;
  }
  return total;
}

/** The items that match, best first; ties keep their order. */
export function rank<T>(query: string, items: readonly T[], fields: (item: T) => readonly string[]): T[] {
  const scored: { item: T; score: number; index: number }[] = [];
  items.forEach((item, index) => {
    const score = scoreFields(query, fields(item));
    if (score !== null) scored.push({ item, score, index });
  });
  scored.sort((a, b) => b.score - a.score || a.index - b.index);
  return scored.map((s) => s.item);
}
