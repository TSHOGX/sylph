/**
 * Subsequence fuzzy matching for the palette and search overlays.
 *
 * # Why not fuse.js
 *
 * Fuse is ~12 KB gzipped and implements Bitap with configurable field weighting
 * and typo tolerance. What a command palette needs is the narrower thing every
 * editor palette does: match the query as a subsequence, and rank by how tightly
 * the matched characters cluster. That is what makes `nsn` find "New Session" and
 * rank it above "Nested Snapshot".
 *
 * Typo tolerance is deliberately absent. In a palette the candidate list is
 * short, visible, and arrow-navigable, so a near-miss costs one keystroke to fix
 * — while fuzzy typo matching regularly promotes an unrelated command to the top
 * of the list, which is the failure that actually hurts.
 */

export interface FuzzyMatch {
  score: number;
  /** Indices in the candidate that matched, for highlighting. */
  indices: number[];
}

/**
 * Score a candidate against a query.
 *
 * Returns `null` when the query is not a subsequence of the candidate. A higher
 * score is a better match; scores are only comparable within one query.
 */
export function fuzzyMatch(candidate: string, query: string): FuzzyMatch | null {
  if (!query) return { score: 0, indices: [] };

  const haystack = candidate.toLowerCase();
  const needle = query.toLowerCase();

  // Substring matches are the common case and always beat a scattered
  // subsequence, so they short-circuit with a large bonus. A match at a word
  // boundary scores higher still: typing "ses" should prefer "New Session" over
  // "Uses Cache".
  const direct = haystack.indexOf(needle);
  if (direct !== -1) {
    const boundary = direct === 0 || /[\s/\-_.]/.test(haystack[direct - 1]);
    const score =
      1000 - direct * 2 + (boundary ? 200 : 0) + Math.round((needle.length / haystack.length) * 100);
    return {
      score,
      indices: Array.from({ length: needle.length }, (_, i) => direct + i),
    };
  }

  const indices: number[] = [];
  let at = 0;
  let score = 0;
  let previous = -2;

  for (const character of needle) {
    const found = haystack.indexOf(character, at);
    if (found === -1) return null;

    // Adjacency is what distinguishes a real acronym match from characters that
    // happen to appear in order across the whole string.
    if (found === previous + 1) score += 15;
    // A match at the start of a word is a strong signal: `ns` over `New Session`.
    if (found === 0 || /[\s/\-_.]/.test(haystack[found - 1])) score += 25;
    // Distance from the previous match is a penalty, so tight clusters win.
    score -= Math.min(found - previous - 1, 10);

    indices.push(found);
    previous = found;
    at = found + 1;
  }

  // Shorter candidates matching the same query are more likely the intended one.
  score += Math.max(0, 40 - haystack.length / 2);
  return { score, indices };
}

/**
 * Rank items by their best-matching field.
 *
 * `fields` is ordered by importance: an earlier field's match scores higher, so a
 * query hitting a session's title outranks the same query hitting its path.
 */
export function fuzzyRank<T>(
  items: T[],
  query: string,
  fields: (item: T) => string[],
): Array<{ item: T; score: number; field: number; indices: number[] }> {
  const results: Array<{ item: T; score: number; field: number; indices: number[] }> = [];

  for (const item of items) {
    // A plain `for` loop rather than `forEach`: TypeScript cannot see through a
    // callback that a closed-over `let` was assigned, so the narrowing after the
    // loop would be wrong.
    const values = fields(item);
    let best: { score: number; field: number; indices: number[] } | undefined;
    for (let index = 0; index < values.length; index += 1) {
      const match = fuzzyMatch(values[index], query);
      if (!match) continue;
      // Each later field is worth progressively less, which is what keeps a
      // path match from outranking a title match of equal tightness.
      const weighted = match.score - index * 60;
      if (best === undefined || weighted > best.score) {
        best = { score: weighted, field: index, indices: match.indices };
      }
    }
    if (best !== undefined) results.push({ item, ...best });
  }

  return results.sort((a, b) => b.score - a.score);
}
