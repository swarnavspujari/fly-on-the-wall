//! What a person typed → search terms and FTS5 expressions.
//!
//! Two kinds of query reach search:
//! * **keywords** (`pricing enterprise`, `"usage based" pricing`): precise.
//!   Every word must match (AND), the last word also as a prefix
//!   (search-as-you-type), and quoted phrases match as phrases.
//! * **questions** ("What did Ines ask for in the first launch meeting?"):
//!   no single row holds every word of a question, so AND finds nothing.
//!
//! [`FtsQuery`] carries both forms. `all` is the AND form above: for a
//! query of up to [`KEYWORD_MAX_CONTENT_WORDS`] content words every word
//! typed, stopwords included ("IT budget", "Will's notes"), exactly as
//! before; for a longer query its content words. `any` ORs the content
//! terms (a term of four or more characters also matches as a prefix, so
//! "budget" finds "budgets"), with quoted phrases still required, and
//! relies on bm25 to rank rows that share the rarest words first. Callers
//! try `all` first and fall back to `any` only when `all` matches nothing,
//! so a short keyword search finds what it found before and a question
//! still finds what shares its words.
//!
//! Every user word reaches FTS5 inside double quotes (with any quote of its
//! own removed, and control characters dropped), so no input can inject
//! FTS5 syntax. Typographic double quotes delimit phrases like straight
//! ones.

use std::collections::HashSet;

/// Words with no retrieval value: articles, pronouns, auxiliaries, question
/// words and contraction stubs ("don't" tokenizes as "don" + "t").
pub const STOPWORDS: &[&str] = &[
    "a", "about", "after", "again", "all", "also", "am", "an", "and", "any", "are", "aren", "as",
    "at", "be", "been", "before", "being", "both", "but", "by", "can", "could", "couldn", "did",
    "didn", "do", "does", "doesn", "doing", "don", "each", "for", "from", "had", "hadn", "has",
    "hasn", "have", "haven", "having", "he", "her", "here", "hers", "him", "his", "how", "i", "if",
    "in", "into", "is", "isn", "it", "its", "just", "ll", "me", "my", "of", "on", "or", "our",
    "ours", "re", "s", "say", "said", "says", "she", "should", "shouldn", "so", "some", "t",
    "than", "that", "the", "their", "theirs", "them", "then", "there", "these", "they", "this",
    "those", "to", "too", "us", "ve", "very", "was", "wasn", "we", "were", "weren", "what", "when",
    "where", "which", "while", "who", "whom", "why", "will", "with", "won", "would", "wouldn",
    "you", "your", "yours",
];

pub fn is_stopword(word: &str) -> bool {
    STOPWORDS.contains(&word)
}

/// Lowercase alphanumeric words of `text` with diacritics folded, in order
/// (no filtering).
pub fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut out = String::new();
            fold_word_into(w, &mut out);
            out
        })
}

/// One word as [`words`] yields it (lowercase, diacritics folded), written
/// into `out` (cleared first), so a caller scanning many words reuses one
/// buffer.
pub fn fold_word_into(word: &str, out: &mut String) {
    out.clear();
    out.extend(
        word.chars()
            .flat_map(char::to_lowercase)
            .map(fold_diacritic),
    );
}

/// A lowercase letter without its diacritic, as the FTS tokenizer folds it
/// (`unicode61 remove_diacritics 2`), so in-memory matching agrees with FTS:
/// "ines" matches "Inês". Latin letters with a canonical decomposition only
/// (the tokenizer leaves "ø", "ł" and "ß" alone too).
fn fold_diacritic(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'ţ' | 'ť' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        c => c,
    }
}

/// A word worth searching for: not a stopword, and not a lone letter
/// (a lone digit stays: "5 percent").
fn is_content(word: &str) -> bool {
    !is_stopword(word) && (word.chars().count() > 1 || word.chars().all(|c| c.is_ascii_digit()))
}

/// A query with at most this many content words is a keyword search:
/// every word typed counts, stopwords too ("IT budget", "Will's notes",
/// "us pricing").
pub const KEYWORD_MAX_CONTENT_WORDS: usize = 2;

/// The content words of a query: its [`words`] minus stopwords and lone
/// letters, deduplicated, in order.
pub fn content_terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    words(query)
        .filter(|w| is_content(w))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

/// The terms a query searches for: its [`content_terms`], except that a
/// keyword search (at most [`KEYWORD_MAX_CONTENT_WORDS`] content words)
/// keeps its stopwords too, so "IT budget" looks for "it" as well. Lone
/// letters never count (a lone digit does: "5 percent").
pub fn query_terms(query: &str) -> Vec<String> {
    let content = content_terms(query);
    if content.len() > KEYWORD_MAX_CONTENT_WORDS {
        return content;
    }
    let mut seen = HashSet::new();
    words(query)
        .filter(|w| w.chars().count() > 1 || w.chars().all(|c| c.is_ascii_digit()))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

/// Terms of four or more characters also match as a word prefix.
pub const PREFIX_MIN_CHARS: usize = 4;

/// Whether a (lowercased) word matches a term: exactly, or as a prefix for
/// terms of [`PREFIX_MIN_CHARS`] or more — the rule the `any` expression's
/// `*` applies, so in-memory scoring agrees with FTS.
pub fn term_matches(term: &str, word: &str) -> bool {
    word == term || (term.chars().count() >= PREFIX_MIN_CHARS && word.starts_with(term))
}

/// The FTS5 expression for one term from [`query_terms`] (alphanumeric, so
/// it can't contain a quote): exact, or a prefix per [`term_matches`].
pub fn term_expr(term: &str) -> String {
    if term.chars().count() >= PREFIX_MIN_CHARS {
        format!("\"{term}\"*")
    } else {
        format!("\"{term}\"")
    }
}

/// Per-term document statistics for keyword ranking (IDF): how many rows
/// are in scope, and per term which owners (meetings or notes) hold it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TermStats {
    /// Rows in scope: segments ([`crate::Storage::segment_term_stats`]) or
    /// notes ([`crate::Storage::note_term_stats`]).
    pub docs: usize,
    /// One entry per term, in the order asked.
    pub terms: Vec<TermOwners>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TermOwners {
    /// Rows holding the term.
    pub count: usize,
    /// Owner id → rows of that owner holding the term.
    pub owners: std::collections::HashMap<String, usize>,
}

impl TermOwners {
    pub(crate) fn collect(
        rows: impl Iterator<Item = rusqlite::Result<(String, usize)>>,
    ) -> rusqlite::Result<Self> {
        let mut t = Self::default();
        for row in rows {
            let (owner, n) = row?;
            t.count += n;
            *t.owners.entry(owner).or_default() += n;
        }
        Ok(t)
    }
}

impl TermStats {
    /// BM25's inverse document frequency of each term
    /// (`ln(1 + (N - df + 0.5) / (df + 0.5))`): near 0 for a term in every
    /// row, highest for a term in none.
    pub fn idf(&self) -> Vec<f32> {
        let n = self.docs as f32;
        self.terms
            .iter()
            .map(|t| {
                let df = (t.count as f32).min(n);
                (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
            })
            .collect()
    }

    /// [`idf`](Self::idf) for weighing how much of a query a text covers: a
    /// term no row in scope holds (a typo, a misheard name, a word only a
    /// title or speaker label has) weighs like the rarest term that does
    /// occur, not more, so one unknown word can't outweigh the rest of the
    /// query. With no term occurring at all, plain idf.
    pub fn coverage_weights(&self) -> Vec<f32> {
        let idf = self.idf();
        let cap = self
            .terms
            .iter()
            .zip(&idf)
            .filter(|(t, _)| t.count > 0)
            .map(|(_, w)| *w)
            .reduce(f32::max);
        match cap {
            None => idf,
            Some(cap) => idf
                .iter()
                .zip(&self.terms)
                .map(|(w, t)| if t.count == 0 { w.min(cap) } else { *w })
                .collect(),
        }
    }
}

/// The two FTS5 forms of one query (see the module docs). Both are empty
/// for a query with nothing searchable in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtsQuery {
    /// Precise: every phrase and every content word (AND).
    pub all: String,
    /// Broad: every phrase, plus at least one content term (OR). Empty when
    /// it would add nothing over `all`.
    pub any: String,
}

impl FtsQuery {
    pub fn parse(input: &str) -> Self {
        // A NUL would end the string SQLite sees; typographic double quotes
        // delimit phrases like straight ones.
        let input: String = input
            .chars()
            .map(|c| match c {
                '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' | '\u{00AB}'
                | '\u{00BB}' => '"',
                c if c.is_control() => ' ',
                c => c,
            })
            .collect();
        let input = input.as_str();
        let (phrases, rest) = split_phrases(input);
        let tokens: Vec<String> = rest
            .split_whitespace()
            .map(|t| t.replace('"', ""))
            .filter(|t| !t.is_empty())
            .collect();
        let is_content_token = |t: &&String| words(t).any(|w| is_content(&w));
        let kept: Vec<&String> = if content_terms(&rest).len() > KEYWORD_MAX_CONTENT_WORDS {
            tokens.iter().filter(is_content_token).collect()
        } else {
            // A keyword search (or nothing but stopwords: "who"): every
            // word as typed.
            tokens.iter().collect()
        };
        let ends_with_word = !input.trim_end().ends_with('"');
        let mut all: Vec<String> = phrases.iter().map(|p| format!("\"{p}\"")).collect();
        for (i, t) in kept.iter().enumerate() {
            let last = i + 1 == kept.len() && tokens.last() == Some(*t) && ends_with_word;
            all.push(if last {
                format!("\"{t}\"*")
            } else {
                format!("\"{t}\"")
            });
        }
        let all = all.join(" ");

        let terms = content_terms(&rest);
        let any = if terms.len() < 2 && kept.len() < 2 {
            // One word (or none): the AND form is already as broad as it gets.
            String::new()
        } else {
            let ors = terms
                .iter()
                .map(|t| term_expr(t))
                .collect::<Vec<_>>()
                .join(" OR ");
            let mut parts: Vec<String> = phrases.iter().map(|p| format!("\"{p}\"")).collect();
            match terms.len() {
                0 => {}
                1 => parts.push(ors),
                _ if parts.is_empty() => parts.push(ors),
                _ => parts.push(format!("({ors})")),
            }
            let any = parts.join(" AND ");
            if any == all {
                String::new()
            } else {
                any
            }
        };
        Self { all, any }
    }

    /// Nothing searchable.
    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    /// The expression to run: `all` while it matches anything (`matches_all`
    /// asks the index), else `any` when there is one.
    pub fn choose<E>(
        &self,
        matches_all: impl FnOnce(&str) -> std::result::Result<bool, E>,
    ) -> std::result::Result<&str, E> {
        if self.any.is_empty() || matches_all(&self.all)? {
            Ok(&self.all)
        } else {
            Ok(&self.any)
        }
    }
}

/// Pull `"quoted phrases"` out of the input (their inner text, trimmed and
/// non-empty), returning them and the rest. An unbalanced quote is ignored.
fn split_phrases(input: &str) -> (Vec<String>, String) {
    let mut phrases = Vec::new();
    let mut rest = String::new();
    let mut parts = input.split('"');
    if let Some(head) = parts.next() {
        rest.push_str(head);
    }
    let parts: Vec<&str> = parts.collect();
    // With an odd number of quotes the last part is unterminated text.
    let closed = parts.len() - parts.len() % 2;
    for (i, part) in parts.iter().enumerate() {
        if i < closed && i % 2 == 0 {
            let p = part.split_whitespace().collect::<Vec<_>>().join(" ");
            if !p.is_empty() {
                phrases.push(p);
            }
        } else {
            rest.push(' ');
            rest.push_str(part);
        }
    }
    (phrases, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_drop_stopwords_letters_and_repeats() {
        assert_eq!(
            query_terms("What did Dana say about the Q3 budget? The budget!"),
            vec!["dana", "q3", "budget"]
        );
        assert_eq!(
            query_terms("Harrowgate's 5 seats"),
            vec!["harrowgate", "5", "seats"]
        );
        assert!(content_terms("who is it?").is_empty());
    }

    #[test]
    fn short_keyword_queries_keep_their_stopwords() {
        assert_eq!(query_terms("IT budget"), vec!["it", "budget"]);
        assert_eq!(query_terms("Will's feedback"), vec!["will", "feedback"]);
        assert_eq!(query_terms("who is it?"), vec!["who", "is", "it"]);
        assert_eq!(content_terms("IT budget"), vec!["budget"]);
        let q = FtsQuery::parse("IT budget");
        assert_eq!(q.all, "\"IT\" \"budget\"*", "every word, as before");
        assert_eq!(q.any, "\"budget\"*", "the fallback ORs content words only");
        let q = FtsQuery::parse("Will's feedback");
        assert_eq!(q.all, "\"Will's\" \"feedback\"*");
    }

    #[test]
    fn diacritics_fold_like_the_tokenizer() {
        assert_eq!(query_terms("Inês Müller"), vec!["ines", "muller"]);
        assert!(term_matches("ines", &words("Inês").next().unwrap()));
        assert_eq!(
            words("Søren Łódź").collect::<Vec<_>>(),
            vec!["søren", "łodz"]
        );
    }

    #[test]
    fn unseen_terms_weigh_like_the_rarest_seen_one() {
        let owners = |count| TermOwners {
            count,
            owners: Default::default(),
        };
        let stats = TermStats {
            docs: 1000,
            terms: vec![owners(300), owners(40), owners(0)],
        };
        let idf = stats.idf();
        let w = stats.coverage_weights();
        assert_eq!(&w[..2], &idf[..2]);
        assert!(idf[2] > idf[1]);
        assert_eq!(w[2], idf[1], "capped at the rarest seen term");
        // Nothing seen: plain idf.
        let none = TermStats {
            docs: 1000,
            terms: vec![owners(0), owners(0)],
        };
        assert_eq!(none.coverage_weights(), none.idf());
    }

    #[test]
    fn control_characters_and_curly_quotes() {
        let q = FtsQuery::parse("budget\0 review\u{7}");
        assert!(!q.all.chars().any(char::is_control), "{:?}", q.all);
        assert_eq!(q.all, "\"budget\" \"review\"*");
        let q = FtsQuery::parse("\u{201C}usage based\u{201D} pricing");
        assert_eq!(q.all, "\"usage based\" \"pricing\"*");
    }

    #[test]
    fn prefix_rule() {
        assert!(term_matches("budget", "budgets"));
        assert!(term_matches("q3", "q3"));
        assert!(
            !term_matches("q3", "q30"),
            "short terms match whole words only"
        );
        assert!(!term_matches("api", "apis"));
    }

    #[test]
    fn keywords_stay_precise() {
        let q = FtsQuery::parse("pricing enterpr");
        assert_eq!(q.all, "\"pricing\" \"enterpr\"*");
        assert_eq!(q.any, "\"pricing\"* OR \"enterpr\"*");
        // A single word has no broader form.
        let q = FtsQuery::parse("budget");
        assert_eq!(q.all, "\"budget\"*");
        assert_eq!(q.any, "");
    }

    #[test]
    fn questions_get_an_or_form_without_stopwords() {
        let q = FtsQuery::parse("What did Ines ask for in the launch meeting?");
        assert_eq!(q.all, "\"Ines\" \"ask\" \"launch\" \"meeting?\"*");
        assert_eq!(q.any, "\"ines\"* OR \"ask\" OR \"launch\"* OR \"meeting\"*");
    }

    #[test]
    fn phrases_stay_required() {
        let q = FtsQuery::parse("\"usage based\" pricing for enterprise");
        assert_eq!(
            q.all, "\"usage based\" \"pricing\" \"for\" \"enterprise\"*",
            "two content words: a keyword search, every word as typed"
        );
        assert_eq!(
            q.any,
            "\"usage based\" AND (\"pricing\"* OR \"enterprise\"*)"
        );
        let q = FtsQuery::parse("\"usage based\"");
        assert_eq!(q.all, "\"usage based\"");
        assert_eq!(q.any, "");
        let q = FtsQuery::parse("\"usage based\" pricing");
        assert_eq!(q.any, "", "one term after the phrase: AND already");
    }

    #[test]
    fn hostile_and_degenerate_input() {
        assert!(FtsQuery::parse("   ").is_empty());
        assert!(FtsQuery::parse("\"\"").is_empty());
        let q = FtsQuery::parse("who");
        assert_eq!(q.all, "\"who\"*", "stopwords alone are still searchable");
        let q = FtsQuery::parse("AND OR NOT");
        assert_eq!(
            q.all, "\"AND\" \"OR\" \"NOT\"*",
            "operators are only ever words"
        );
        let q = FtsQuery::parse("and or");
        assert_eq!(q.all, "\"and\" \"or\"*");
        let q = FtsQuery::parse("\"unbalanced budget");
        assert_eq!(q.all, "\"unbalanced\" \"budget\"*");
        for input in [
            "col:val budget",
            "a*b(c) d",
            "x\" OR \"y budget",
            "NEAR(a b) what is",
            "^start - minus",
        ] {
            let q = FtsQuery::parse(input);
            for e in [&q.all, &q.any] {
                // Every user word sits inside a quoted string: outside the
                // quotes there are only our operators.
                assert_eq!(e.matches('"').count() % 2, 0, "{input:?} → {e:?}");
                let outside = e.split('"').step_by(2).collect::<Vec<_>>().join(" ");
                for w in outside
                    .split(|c: char| c.is_whitespace() || "()*".contains(c))
                    .filter(|w| !w.is_empty())
                {
                    assert!(w == "AND" || w == "OR", "{input:?} → {e:?}");
                }
            }
        }
    }
}
