//! The booru content rating: safe, questionable or explicit.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Rating {
    #[serde(rename = "s")]
    Safe,
    #[serde(rename = "q")]
    Questionable,
    #[serde(rename = "e")]
    Explicit,
}

impl Rating {
    pub const ALL: [Rating; 3] = [Rating::Safe, Rating::Questionable, Rating::Explicit];

    /// Accepts the short and long forms, any case: `s`, `safe`, `q`, `questionable`, `e`, `explicit`.
    pub fn parse(s: &str) -> Option<Rating> {
        match s.trim().to_ascii_lowercase().as_str() {
            "s" | "safe" => Some(Rating::Safe),
            "q" | "questionable" => Some(Rating::Questionable),
            "e" | "explicit" => Some(Rating::Explicit),
            _ => None,
        }
    }

    /// The one-letter form stored in files: `s`, `q` or `e`.
    pub fn code(self) -> &'static str {
        match self {
            Rating::Safe => "s",
            Rating::Questionable => "q",
            Rating::Explicit => "e",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Rating::Safe => 0,
            Rating::Questionable => 1,
            Rating::Explicit => 2,
        }
    }
}

impl fmt::Display for Rating {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Rating::Safe => "safe",
            Rating::Questionable => "questionable",
            Rating::Explicit => "explicit",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_and_long_forms() {
        assert_eq!(Rating::parse("S"), Some(Rating::Safe));
        assert_eq!(Rating::parse("questionable"), Some(Rating::Questionable));
        assert_eq!(Rating::parse(" e "), Some(Rating::Explicit));
        assert_eq!(Rating::parse("x"), None);
        for r in Rating::ALL {
            assert_eq!(Rating::parse(r.code()), Some(r));
            assert_eq!(Rating::parse(&r.to_string()), Some(r));
        }
    }
}
