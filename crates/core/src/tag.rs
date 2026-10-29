//! Tag names.
//!
//! A tag is a lowercase string without whitespace, like `cat`, `blue_sky` or `artist:foo`. The
//! part before the first `:` is a namespace only if the tag list defines that namespace, so
//! `re:zero` stays a plain tag unless `re` is a namespace.

use thiserror::Error;

/// Longest tag name, in bytes.
pub const MAX_TAG_LEN: usize = 200;
/// Longest namespace name, in bytes.
pub const MAX_NAMESPACE_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TagError {
    #[error("tag is empty")]
    Empty,
    #[error("tag is longer than {MAX_TAG_LEN} bytes")]
    TooLong,
    #[error("tag `{0}` can't start with `-` or `~` (they mean NOT and OR in searches)")]
    BadStart(String),
    #[error("tag `{0}` can't contain `*` (it is the search wildcard)")]
    Wildcard(String),
    #[error("tag `{0}` contains a control character")]
    Control(String),
    #[error("tag `{0}` can't start or end with `:`")]
    Colon(String),
}

/// Turns user input into a tag name: trims it, lowercases it and joins words with `_`.
/// `"  Blue Sky "` becomes `blue_sky`.
pub fn normalize_tag(input: &str) -> Result<String, TagError> {
    let lower = input.trim().to_lowercase();
    let tag = lower.split_whitespace().collect::<Vec<_>>().join("_");
    if tag.is_empty() {
        return Err(TagError::Empty);
    }
    if tag.len() > MAX_TAG_LEN {
        return Err(TagError::TooLong);
    }
    if tag.starts_with('-') || tag.starts_with('~') {
        return Err(TagError::BadStart(tag));
    }
    if tag.contains('*') {
        return Err(TagError::Wildcard(tag));
    }
    if tag.chars().any(char::is_control) {
        return Err(TagError::Control(tag));
    }
    if tag.starts_with(':') || tag.ends_with(':') {
        return Err(TagError::Colon(tag));
    }
    Ok(tag)
}

/// A namespace name is 1–32 characters of `a-z`, `0-9` and `_`, and not a search metatag name.
pub fn is_valid_namespace(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAMESPACE_LEN
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !crate::METATAGS.contains(&name)
}

/// Splits `artist:foo` into `(Some("artist"), "foo")` if `is_namespace("artist")`; otherwise
/// returns `(None, tag)`.
pub fn split_namespace(tag: &str, is_namespace: impl Fn(&str) -> bool) -> (Option<&str>, &str) {
    match tag.split_once(':') {
        Some((ns, name)) if !name.is_empty() && is_namespace(ns) => (Some(ns), name),
        _ => (None, tag),
    }
}

/// Matches `text` against a pattern where `*` stands for any run of characters (including none).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else { return pattern == text };
    if parts.len() == 1 {
        return pattern == text;
    }
    if !text.starts_with(first) {
        return false;
    }
    let mut rest = &text[first.len()..];
    let middle = parts.get(1..parts.len() - 1).unwrap_or(&[]);
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_and_spaces() {
        assert_eq!(normalize_tag("  Blue  Sky ").unwrap(), "blue_sky");
        assert_eq!(normalize_tag("Artist:Foo").unwrap(), "artist:foo");
        assert_eq!(normalize_tag("ÉCOLE").unwrap(), "école");
    }

    #[test]
    fn rejects_bad_tags() {
        assert_eq!(normalize_tag("   "), Err(TagError::Empty));
        assert!(matches!(normalize_tag("-cat"), Err(TagError::BadStart(_))));
        assert!(matches!(normalize_tag("~cat"), Err(TagError::BadStart(_))));
        assert!(matches!(normalize_tag("ca*t"), Err(TagError::Wildcard(_))));
        assert!(matches!(normalize_tag("ca\u{7}t"), Err(TagError::Control(_))));
        assert!(matches!(normalize_tag(":cat"), Err(TagError::Colon(_))));
        assert!(matches!(normalize_tag("cat:"), Err(TagError::Colon(_))));
        assert_eq!(normalize_tag(&"a".repeat(MAX_TAG_LEN + 1)), Err(TagError::TooLong));
    }

    #[test]
    fn namespaces() {
        assert!(is_valid_namespace("artist"));
        assert!(!is_valid_namespace("Artist"));
        assert!(!is_valid_namespace("rating"));
        assert!(!is_valid_namespace(""));
        let ns = |n: &str| n == "artist";
        assert_eq!(split_namespace("artist:foo", ns), (Some("artist"), "foo"));
        assert_eq!(split_namespace("re:zero", ns), (None, "re:zero"));
        assert_eq!(split_namespace("cat", ns), (None, "cat"));
    }

    #[test]
    fn globs() {
        assert!(glob_match("cat*", "cat"));
        assert!(glob_match("cat*", "cat_ears"));
        assert!(glob_match("*ears", "cat_ears"));
        assert!(glob_match("c*t", "cat"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "aXbYc"));
        assert!(!glob_match("a*b*c", "aXcYb"));
        assert!(!glob_match("ab*ba", "aba"));
        assert!(!glob_match("cat", "cats"));
        assert!(glob_match("é*", "école"));
    }
}
