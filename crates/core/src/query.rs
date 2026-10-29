//! The booru search language.
//!
//! A query is a list of whitespace-separated terms:
//!
//! - `cat` — must have the tag; `-cat` — must not; `~cat ~dog` — at least one of the `~` terms.
//! - `cat*` — wildcard: any tag matching the pattern.
//! - `rating:s`, `rating:q,e` — metatags (more arrive in M2: width, date, order…).
//!
//! The parser reports errors with byte spans so a search bar can underline them.

use std::ops::Range;

use thiserror::Error;

use crate::rating::Rating;
use crate::tag::normalize_tag;

/// Longest query accepted, in bytes.
pub const MAX_QUERY_LEN: usize = 8192;
/// Most terms in one query.
pub const MAX_TERMS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Plain term: must match.
    Must,
    /// `-term`: must not match.
    Not,
    /// `~term`: at least one `~` term must match.
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagPattern {
    Exact(String),
    /// Contains `*`.
    Wildcard(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermKind {
    Tag(TagPattern),
    /// Any of these ratings.
    Rating(Vec<Rating>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub mode: Mode,
    pub kind: TermKind,
    /// Byte range of the term in the query text.
    pub span: Range<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<Term>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct QueryError {
    pub message: String,
    /// Byte range to underline.
    pub span: Range<usize>,
}

fn err(message: impl Into<String>, span: Range<usize>) -> QueryError {
    QueryError { message: message.into(), span }
}

/// Splits on whitespace, keeping byte offsets.
fn tokens(input: &str) -> impl Iterator<Item = (&str, Range<usize>)> {
    let mut start = None;
    let mut out = Vec::new();
    for (i, c) in input.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                out.push((&input[s..i], s..i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((&input[s..], s..input.len()));
    }
    out.into_iter()
}

impl Query {
    pub fn parse(input: &str) -> Result<Query, QueryError> {
        if input.len() > MAX_QUERY_LEN {
            return Err(err(format!("query is longer than {MAX_QUERY_LEN} bytes"), 0..input.len()));
        }
        let mut terms = Vec::new();
        for (token, span) in tokens(input) {
            if terms.len() >= MAX_TERMS {
                return Err(err(format!("more than {MAX_TERMS} terms"), span));
            }
            terms.push(parse_term(token, span)?);
        }
        Ok(Query { terms })
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

fn parse_term(token: &str, span: Range<usize>) -> Result<Term, QueryError> {
    let (mode, body) = if let Some(rest) = token.strip_prefix('-') {
        (Mode::Not, rest)
    } else if let Some(rest) = token.strip_prefix('~') {
        (Mode::Any, rest)
    } else {
        (Mode::Must, token)
    };
    if body.is_empty() {
        return Err(err(format!("expected a tag after `{token}`"), span));
    }
    if let Some((name, value)) = body.split_once(':') {
        let name = name.to_ascii_lowercase();
        if crate::METATAGS.contains(&name.as_str()) {
            let kind = parse_metatag(&name, value, &span)?;
            return Ok(Term { mode, kind, span });
        }
    }
    let lower = body.to_lowercase();
    let pattern = if lower.contains('*') {
        if lower.starts_with('-') || lower.starts_with('~') {
            return Err(err(format!("`{body}`: only one of `-` and `~` can start a term"), span));
        }
        TagPattern::Wildcard(lower)
    } else {
        TagPattern::Exact(normalize_tag(&lower).map_err(|e| err(e.to_string(), span.clone()))?)
    };
    Ok(Term { mode, kind: TermKind::Tag(pattern), span })
}

fn parse_metatag(name: &str, value: &str, span: &Range<usize>) -> Result<TermKind, QueryError> {
    match name {
        "rating" => {
            let mut ratings = Vec::new();
            for part in value.split(',') {
                let r = Rating::parse(part).ok_or_else(|| err(format!("unknown rating `{part}`; use s, q or e"), span.clone()))?;
                if !ratings.contains(&r) {
                    ratings.push(r);
                }
            }
            Ok(TermKind::Rating(ratings))
        }
        _ => Err(err(format!("`{name}:` searches aren't supported yet"), span.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(q: &str) -> Vec<(Mode, TermKind)> {
        Query::parse(q).unwrap().terms.into_iter().map(|t| (t.mode, t.kind)).collect()
    }

    fn tag(s: &str) -> TermKind {
        TermKind::Tag(TagPattern::Exact(s.into()))
    }

    #[test]
    fn plain_negated_and_or_terms() {
        assert_eq!(
            kinds("  Cat -dog ~sky ~SEA artist:Foo  "),
            vec![(Mode::Must, tag("cat")), (Mode::Not, tag("dog")), (Mode::Any, tag("sky")), (Mode::Any, tag("sea")), (Mode::Must, tag("artist:foo")),]
        );
        assert!(Query::parse("   ").unwrap().is_empty());
    }

    #[test]
    fn wildcards_and_ratings() {
        assert_eq!(
            kinds("cat* -rating:e"),
            vec![(Mode::Must, TermKind::Tag(TagPattern::Wildcard("cat*".into()))), (Mode::Not, TermKind::Rating(vec![Rating::Explicit]))]
        );
        assert_eq!(kinds("rating:q,E,q"), vec![(Mode::Must, TermKind::Rating(vec![Rating::Questionable, Rating::Explicit]))]);
    }

    #[test]
    fn spans_point_at_the_bad_term() {
        let e = Query::parse("cat rating:x").unwrap_err();
        assert_eq!(e.span, 4..12);
        let e = Query::parse("cat - dog").unwrap_err();
        assert_eq!(e.span, 4..5);
        let e = Query::parse("ok width:>5").unwrap_err();
        assert_eq!(e.span, 3..11);
        let e = Query::parse("é --x").unwrap_err();
        assert_eq!(e.span, 3..6);
    }

    #[test]
    fn limits() {
        assert!(Query::parse(&"a ".repeat(MAX_TERMS + 1)).is_err());
        assert!(Query::parse(&"a".repeat(MAX_QUERY_LEN + 1)).is_err());
    }
}
