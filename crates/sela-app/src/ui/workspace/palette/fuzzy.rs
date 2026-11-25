//! Fuzzy subsequence match and ranking for the command palette (PAL-04).

/// Indices of matched characters in the haystack (for highlight).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FuzzyMatch {
    pub indices: Vec<usize>,
    /// Lower is better. Used after recent / prefix tiers.
    pub score: i32,
}

/// Case-insensitive subsequence match. Returns match indices into `haystack`.
pub(crate) fn fuzzy_match(query: &str, haystack: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            indices: Vec::new(),
            score: 0,
        });
    }
    let hay_chars: Vec<(usize, char)> = haystack
        .char_indices()
        .map(|(i, c)| (i, c.to_ascii_lowercase()))
        .collect();
    let mut indices = Vec::with_capacity(query.chars().count());
    let mut hi = 0usize;
    for qc in query.chars().map(|c| c.to_ascii_lowercase()) {
        let mut found = false;
        while hi < hay_chars.len() {
            let (byte_ix, hc) = hay_chars[hi];
            hi += 1;
            if hc == qc {
                indices.push(byte_ix);
                found = true;
                break;
            }
        }
        if !found {
            return None;
        }
    }
    // Prefer early, contiguous matches.
    let first = indices.first().copied().unwrap_or(0) as i32;
    let span = indices
        .last()
        .zip(indices.first())
        .map(|(a, b)| (*a as i32) - (*b as i32))
        .unwrap_or(0);
    let score = first * 10 + span;
    Some(FuzzyMatch { indices, score })
}

/// Ranking tier for palette rows (lower sorts first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RankTier {
    Recent = 0,
    Prefix = 1,
    Fuzzy = 2,
    Other = 3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ranked<'a, T> {
    pub tier: RankTier,
    pub score: i32,
    pub label: &'a str,
    pub item: T,
    pub indices: Vec<usize>,
}

/// Rank `items` by recent → case-insensitive prefix → fuzzy subsequence → others.
/// Empty query keeps input order (caller should already put recent first).
pub(crate) fn rank_items<'a, T>(
    query: &str,
    items: impl IntoIterator<Item = (T, &'a str, bool)>,
) -> Vec<Ranked<'a, T>>
where
    T: Clone,
{
    // items: (payload, label, is_recent)
    let q = query.trim();
    let mut out: Vec<Ranked<'a, T>> = Vec::new();
    for (item, label, is_recent) in items {
        if q.is_empty() {
            out.push(Ranked {
                tier: if is_recent {
                    RankTier::Recent
                } else {
                    RankTier::Other
                },
                score: 0,
                label,
                item,
                indices: Vec::new(),
            });
            continue;
        }
        let Some(m) = fuzzy_match(q, label) else {
            continue;
        };
        let lower = label.to_ascii_lowercase();
        let q_lower = q.to_ascii_lowercase();
        let exact = lower == q_lower;
        let prefix = lower.starts_with(&q_lower);
        let tier = if is_recent {
            RankTier::Recent
        } else if prefix {
            RankTier::Prefix
        } else {
            RankTier::Fuzzy
        };
        // Exact name wins over a neighbour within the same tier.
        let score = if exact { m.score - 10_000 } else { m.score };
        out.push(Ranked {
            tier,
            score,
            label,
            item,
            indices: m.indices,
        });
    }
    out.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then(a.score.cmp(&b.score))
            .then(a.label.cmp(b.label))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        let m = fuzzy_match("", "public.books").unwrap();
        assert!(m.indices.is_empty());
    }

    #[test]
    fn subsequence_highlights_matched_bytes() {
        let m = fuzzy_match("pbk", "public.books").unwrap();
        let hay = "public.books";
        let matched: String = m
            .indices
            .iter()
            .map(|&i| hay[i..].chars().next().unwrap())
            .collect();
        assert_eq!(matched.to_ascii_lowercase(), "pbk");
    }

    #[test]
    fn ranking_recent_then_prefix_then_fuzzy() {
        let items = [
            ("other", "other.books", false),
            ("recent", "public.books", true),
            ("prefix", "pub", false),
            ("fuzzy", "xpublicy", false),
        ];
        let ranked = rank_items("pub", items);
        assert_eq!(ranked[0].item, "recent");
        assert_eq!(ranked[0].tier, RankTier::Recent);
        assert_eq!(ranked[1].item, "prefix");
        assert_eq!(ranked[1].tier, RankTier::Prefix);
        assert!(ranked.iter().any(|r| r.item == "fuzzy"));
        assert!(!ranked.iter().any(|r| r.item == "other"));
    }

    #[test]
    fn exact_name_beats_neighbour() {
        let items = [("near", "bookstore", false), ("exact", "books", false)];
        let ranked = rank_items("books", items);
        assert_eq!(ranked[0].item, "exact");
    }

    #[test]
    fn schema_table_labels_disambiguate() {
        let a = fuzzy_match("public.books", "public.books").unwrap();
        let b = fuzzy_match("other.books", "other.books").unwrap();
        assert!(!a.indices.is_empty());
        assert!(!b.indices.is_empty());
        let ranked = rank_items(
            "books",
            [("p", "public.books", false), ("o", "other.books", false)],
        );
        assert_eq!(ranked.len(), 2);
    }
}
