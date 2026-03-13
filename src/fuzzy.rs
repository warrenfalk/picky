pub fn score_fields(query: &str, fields: &[(&str, i64)]) -> Option<i64> {
    let terms = split_terms(query);
    if terms.is_empty() {
        return Some(0);
    }

    let mut total_score = 0;

    for term in terms {
        let mut term_matches = fields
            .iter()
            .filter_map(|(field, bonus)| {
                let term_match = score_term(&term, field)?;
                Some((
                    term_match.score + bonus,
                    term_match.echo_bonus + (*bonus).max(0) / 10,
                ))
            })
            .collect::<Vec<_>>();

        if term_matches.is_empty() {
            return None;
        }

        term_matches.sort_by(|left, right| right.0.cmp(&left.0));
        total_score += term_matches[0].0;
        total_score += term_matches
            .iter()
            .skip(1)
            .map(|(_, echo_bonus)| *echo_bonus)
            .sum::<i64>();
    }

    Some(total_score)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn score(query: &str, haystack: &str) -> Option<i64> {
    let terms = split_terms(query);
    if terms.is_empty() {
        return Some(0);
    }

    terms.into_iter()
        .map(|term| score_term(&term, haystack).map(|term_match| term_match.score))
        .sum()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MatchQuality {
    FieldPrefix,
    WordPrefix,
    Substring,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TermMatch {
    score: i64,
    echo_bonus: i64,
    #[allow(dead_code)]
    quality: MatchQuality,
}

fn score_term(term: &str, haystack: &str) -> Option<TermMatch> {
    let term = normalize(term);
    let haystack = normalize(haystack);

    if term.is_empty() {
        return Some(TermMatch {
            score: 0,
            echo_bonus: 0,
            quality: MatchQuality::Substring,
        });
    }

    let haystack_len = haystack.chars().count() as i64;
    let term_len = term.chars().count() as i64;
    let length_penalty = (haystack_len - term_len).max(0) / 4;

    if let Some(index) = haystack.find(&term) {
        let position_penalty = char_position(&haystack, index) as i64;
        if index == 0 {
            return Some(TermMatch {
                score: 180 - position_penalty - length_penalty,
                echo_bonus: 18,
                quality: MatchQuality::FieldPrefix,
            });
        }
    }

    if let Some(index) = word_prefix_index(&haystack, &term) {
        let position_penalty = char_position(&haystack, index) as i64;
        return Some(TermMatch {
            score: 120 - position_penalty - length_penalty,
            echo_bonus: 12,
            quality: MatchQuality::WordPrefix,
        });
    }

    haystack.find(&term).map(|index| {
        let position_penalty = char_position(&haystack, index) as i64;
        TermMatch {
            score: 70 - position_penalty - length_penalty,
            echo_bonus: 6,
            quality: MatchQuality::Substring,
        }
    })
}

fn split_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(normalize)
        .filter(|term| !term.is_empty())
        .collect()
}

fn normalize(input: &str) -> String {
    input.chars().flat_map(|ch| ch.to_lowercase()).collect()
}

fn word_prefix_index(haystack: &str, needle: &str) -> Option<usize> {
    haystack.match_indices(needle).find_map(|(index, _)| {
        if index == 0 || previous_char_is_boundary(haystack, index) {
            Some(index)
        } else {
            None
        }
    })
}

fn previous_char_is_boundary(haystack: &str, index: usize) -> bool {
    haystack[..index]
        .chars()
        .next_back()
        .map(is_word_boundary)
        .unwrap_or(false)
}

fn is_word_boundary(ch: char) -> bool {
    matches!(ch, ' ' | '-' | '_' | '/' | '.' | ':')
}

fn char_position(haystack: &str, byte_index: usize) -> usize {
    haystack[..byte_index].chars().count()
}

#[cfg(test)]
mod tests {
    use super::{score, score_fields};

    #[test]
    fn empty_query_matches_with_zero_score() {
        assert_eq!(score("", "anything"), Some(0));
        assert_eq!(score_fields("   ", &[("anything", 0)]), Some(0));
    }

    #[test]
    fn all_terms_must_match_some_field() {
        assert!(score_fields("fire dp-3", &[("Firefox", 0), ("DP-3", 0)]).is_some());
        assert_eq!(score_fields("fire dp-4", &[("Firefox", 0), ("DP-3", 0)]), None);
    }

    #[test]
    fn word_prefix_beats_plain_substring() {
        let word_prefix = score("term", "terminal window").unwrap();
        let substring = score("term", "midterminal window").unwrap();
        assert!(word_prefix > substring);
    }

    #[test]
    fn field_prefix_beats_later_word_prefix() {
        let field_prefix = score("fire", "firefox developer edition").unwrap();
        let later_word_prefix = score("fire", "browser firefox").unwrap();
        assert!(field_prefix > later_word_prefix);
    }

    #[test]
    fn multiple_field_matches_outrank_single_field_match() {
        let single = score_fields("fox", &[("Fox", 0)]).unwrap();
        let multiple = score_fields("fox", &[("Fox", 0), ("Firefox", 0)]).unwrap();
        assert!(multiple > single);
    }
}
