//! Shared BM25 ranking for tool and skill search.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// BM25 term-frequency saturation.
const BM25_K1: f64 = 1.2;

/// BM25 length-normalization factor.
const BM25_B: f64 = 0.75;

/// Case folding used when tokenizing a search query and its documents.
#[derive(Clone, Copy)]
pub(super) enum CaseFold {
    /// Lowercase only ASCII letters.
    Ascii,
    /// Apply Unicode lowercase mapping.
    Unicode,
}

/// Rank candidate documents for a query.
///
/// `exact_bonus` adds each caller's exact-name preference, and `tie_break`
/// orders equal scores before the result limit is applied.
#[expect(
    clippy::cast_precision_loss,
    reason = "the tool and skill catalogs are small enough that f64 represents their sizes exactly"
)]
pub(super) fn rank<'a, T>(
    candidates: Vec<(&'a T, Vec<String>)>,
    query: &str,
    fold: CaseFold,
    limit: usize,
    exact_bonus: impl Fn(&T) -> f64,
    tie_break: impl Fn(&T, &T) -> Ordering,
) -> Vec<&'a T> {
    let query_terms = tokens(query, fold);
    if query_terms.is_empty() || candidates.is_empty() {
        return Vec::new();
    }
    let average_length = candidates
        .iter()
        .map(|(_, tokens)| tokens.len())
        .sum::<usize>() as f64
        / candidates.len() as f64;
    let document_count = candidates.len() as f64;
    let document_frequency =
        term_document_frequency(candidates.iter().map(|(_, tokens)| tokens.as_slice()));
    let mut scored: Vec<(f64, &T)> = candidates
        .into_iter()
        .filter_map(|(entry, document)| {
            let score = bm25_score(
                &query_terms,
                &document,
                &document_frequency,
                average_length,
                document_count,
            ) + exact_bonus(entry);
            (score > 0.0).then_some((score, entry))
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| tie_break(left.1, right.1))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, entry)| entry)
        .collect()
}

/// Tokenize a query value, discarding duplicates.
pub(super) fn tokens(value: &str, fold: CaseFold) -> Vec<String> {
    tokens_of(value, fold)
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

/// Tokenize one document field without deduplicating.
pub(super) fn tokens_of(value: &str, fold: CaseFold) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| match fold {
            CaseFold::Ascii => token.to_ascii_lowercase(),
            CaseFold::Unicode => token.to_lowercase(),
        })
        .collect()
}

fn term_document_frequency<'a>(
    documents: impl IntoIterator<Item = &'a [String]>,
) -> HashMap<String, usize> {
    let mut frequencies = HashMap::new();
    for document in documents {
        let unique: HashSet<&String> = document.iter().collect();
        for term in unique {
            *frequencies.entry(term.clone()).or_insert(0) += 1;
        }
    }
    frequencies
}

#[expect(
    clippy::cast_precision_loss,
    clippy::suboptimal_flops,
    reason = "BM25 is a ranking heuristic over a small catalog; clarity wins over fused operations"
)]
fn bm25_score(
    query_terms: &[String],
    document: &[String],
    document_frequency: &HashMap<String, usize>,
    average_length: f64,
    document_count: f64,
) -> f64 {
    if document.is_empty() || average_length <= 0.0 {
        return 0.0;
    }
    let mut score = 0.0;
    for term in query_terms {
        let frequency = document.iter().filter(|token| *token == term).count() as f64;
        if frequency == 0.0 {
            continue;
        }
        let frequency_in_documents = document_frequency.get(term).copied().unwrap_or(0) as f64;
        if frequency_in_documents == 0.0 {
            continue;
        }
        let idf = ((document_count - frequency_in_documents + 0.5)
            / (frequency_in_documents + 0.5))
            .ln_1p();
        let length_ratio = document.len() as f64 / average_length;
        let denominator = frequency + BM25_K1 * (1.0 - BM25_B + BM25_B * length_ratio);
        score += idf * (frequency * (BM25_K1 + 1.0)) / denominator;
    }
    score
}
