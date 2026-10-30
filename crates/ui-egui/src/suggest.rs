//! Autocomplete for the search bar and the tag input (no egui here, so it can be tested).

use boorucraft_core::{METATAG_HELP, METATAGS, metatag_values};
use boorucraft_store::TagList;

/// Most suggestions shown at once.
pub const LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Replaces the word being typed.
    pub insert: String,
    /// Shown in the list.
    pub label: String,
    /// Shown faintly on the right: a count or an example.
    pub detail: String,
    /// The tag, for its namespace colour (`None` for metatags).
    pub tag: Option<String>,
}

/// Splits `-` or `~` off the front of a word.
fn split_prefix(word: &str) -> (&str, &str) {
    match word.chars().next() {
        Some(c @ ('-' | '~')) => word.split_at(c.len_utf8()),
        _ => ("", word),
    }
}

/// Does `tag` start with `needle`, or does its name after a namespace?
fn tag_match(tag: &str, needle: &str) -> bool {
    tag.starts_with(needle) || tag.split_once(':').is_some_and(|(_, name)| name.starts_with(needle))
}

fn tag_suggestion(prefix: &str, tag: &str, count: u64) -> Suggestion {
    Suggestion {
        insert: format!("{prefix}{tag} "),
        label: tag.to_string(),
        detail: if count > 0 { count.to_string() } else { String::new() },
        tag: Some(tag.to_string()),
    }
}

/// Suggestions for the search-bar word `word`. `usage` is tag counts, most used first.
pub fn for_search(word: &str, usage: &[(String, u64)], tags: &TagList) -> Vec<Suggestion> {
    let (prefix, body) = split_prefix(word);
    let needle = body.to_lowercase();
    if needle.is_empty() || needle.contains('*') || needle.contains('"') {
        return Vec::new();
    }
    // Values after a metatag name.
    if let Some((name, value)) = needle.split_once(':')
        && METATAGS.contains(&name)
    {
        // order: takes no prefix.
        let prefix = if name == "order" { "" } else { prefix };
        let pools: Vec<&str> = if name == "pool" { tags.suggest_pools(value, LIMIT) } else { Vec::new() };
        return pools
            .into_iter()
            .chain(metatag_values(name).iter().copied())
            .filter(|v| v.starts_with(value) && *v != value)
            .take(LIMIT)
            .map(|v| Suggestion { insert: format!("{prefix}{name}:{v} "), label: format!("{name}:{v}"), detail: String::new(), tag: None })
            .collect();
    }
    let mut out: Vec<Suggestion> = Vec::new();
    // Full-name prefix matches first, then matches on the part after a namespace.
    for (tag, n) in usage.iter().filter(|(t, _)| t.starts_with(&needle)).chain(usage.iter().filter(|(t, _)| !t.starts_with(&needle) && tag_match(t, &needle))) {
        if out.len() >= LIMIT {
            break;
        }
        out.push(tag_suggestion(prefix, tag, *n));
    }
    let metas = METATAG_HELP.iter().filter(|(name, _)| name.starts_with(&needle) && (prefix.is_empty() || *name != "order"));
    // Metatags go before tags that merely contain the word, but after the used tags above.
    let mut metas: Vec<Suggestion> = metas
        .map(|(name, example)| Suggestion { insert: format!("{prefix}{name}:"), label: format!("{name}:"), detail: (*example).to_string(), tag: None })
        .collect();
    metas.truncate(3);
    let keep = LIMIT.saturating_sub(metas.len());
    out.truncate(keep);
    out.extend(metas);
    // Tags in the list that no open file uses yet.
    for tag in tags.suggest(&needle, LIMIT) {
        if out.len() >= LIMIT {
            break;
        }
        if !out.iter().any(|s| s.tag.as_deref() == Some(tag)) && tag_match(tag, &needle) {
            out.push(tag_suggestion(prefix, tag, 0));
        }
    }
    out
}

/// Suggestions for the tag editor: tags from the central list (including near misses), with
/// their counts. `-` (remove) is kept.
pub fn for_tag_input(word: &str, usage: &[(String, u64)], tags: &TagList) -> Vec<Suggestion> {
    let (prefix, body) = split_prefix(word);
    if body.is_empty() || prefix == "~" {
        return Vec::new();
    }
    let count = |t: &str| usage.iter().find(|(u, _)| u == t).map_or(0, |(_, n)| *n);
    let mut found: Vec<&str> = tags.suggest(body, LIMIT * 2);
    // The list's own ranking (prefix, contains, typo); within the prefix matches, whole-name
    // matches before namespace-part ones, most used first.
    let needle = body.to_lowercase();
    let prefixed = found.iter().take_while(|t| tag_match(t, &needle)).count();
    if let Some(head) = found.get_mut(..prefixed) {
        head.sort_by_key(|t| (!t.starts_with(needle.as_str()), std::cmp::Reverse(count(t))));
    }
    found.into_iter().filter(|t| **t != needle).take(LIMIT).map(|t| tag_suggestion(prefix, t, count(t))).collect()
}

/// Pool names for the "add to pool" box.
pub fn for_pool_input(word: &str, tags: &TagList) -> Vec<Suggestion> {
    if word.is_empty() {
        return Vec::new();
    }
    tags.suggest_pools(word, LIMIT)
        .into_iter()
        .filter(|p| *p != word)
        .map(|p| Suggestion { insert: p.to_string(), label: p.to_string(), detail: String::new(), tag: None })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> TagList {
        let mut t = TagList::in_memory();
        for tag in ["cat", "cat_ears", "catgirl", "artist:catherine", "sky", "cathedral"] {
            t.create(tag, "").unwrap();
        }
        t
    }

    fn usage() -> Vec<(String, u64)> {
        [("sky", 9), ("cat", 5), ("artist:catherine", 3), ("cat_ears", 2), ("catgirl", 1)].iter().map(|(t, n)| (t.to_string(), *n)).collect()
    }

    fn inserts(s: &[Suggestion]) -> Vec<&str> {
        s.iter().map(|s| s.insert.as_str()).collect()
    }

    #[test]
    fn search_suggests_used_tags_then_metatags_then_the_list() {
        let t = list();
        let u = usage();
        let s = for_search("cat", &u, &t);
        assert_eq!(inserts(&s), vec!["cat ", "cat_ears ", "catgirl ", "artist:catherine ", "cathedral "]);
        assert_eq!(s[0].detail, "5");
        let s = for_search("-ca", &u, &t);
        assert_eq!(s[0].insert, "-cat ", "the prefix is kept");
        let s = for_search("wi", &u, &t);
        assert_eq!(inserts(&s), vec!["width:"]);
        assert!(s[0].detail.contains("1920"));
        assert_eq!(inserts(&for_search("rating:", &u, &t)), vec!["rating:s ", "rating:q ", "rating:e ", "rating:s,q ", "rating:q,e "]);
        assert_eq!(inserts(&for_search("~kind:v", &u, &t)), vec!["~kind:video "]);
        assert_eq!(inserts(&for_search("order:-d", &u, &t)), vec!["order:-date "]);
        assert!(for_search("-or", &u, &t).iter().all(|s| !s.insert.contains("order")), "no -order:");
        assert!(for_search("", &u, &t).is_empty());
        assert!(for_search("cat*", &u, &t).is_empty());
        assert!(for_search("kind:video", &u, &t).is_empty(), "nothing left to complete");
        let mut t = t;
        t.create_pool("trip", "").unwrap();
        assert_eq!(inserts(&for_search("pool:", &u, &t)), vec!["pool:trip ", "pool:any ", "pool:none "]);
        assert_eq!(inserts(&for_pool_input("tr", &t)), vec!["trip"]);
    }

    #[test]
    fn tag_input_suggests_from_the_list() {
        let t = list();
        let u = usage();
        let s = for_tag_input("cat", &u, &t);
        assert_eq!(s.first().map(|s| s.insert.as_str()), Some("cat_ears "), "the exact word isn't offered again");
        assert!(s.iter().any(|s| s.label == "artist:catherine"));
        assert_eq!(for_tag_input("-sk", &u, &t)[0].insert, "-sky ");
        assert_eq!(for_tag_input("skyy", &u, &t)[0].label, "sky", "typos still find the tag");
        assert!(for_tag_input("~sky", &u, &t).is_empty());
    }
}
