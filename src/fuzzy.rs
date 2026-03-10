pub fn score_fields(query: &str, fields: &[(&str, i64)]) -> Option<i64> {
    if query.trim().is_empty() {
        return Some(0);
    }

    fields
        .iter()
        .filter_map(|(field, bonus)| score(query, field).map(|value| value + bonus))
        .max()
}

pub fn score(query: &str, haystack: &str) -> Option<i64> {
    let query = normalize(query);
    let haystack = normalize(haystack);

    if query.is_empty() {
        return Some(0);
    }

    let mut next_index = 0usize;
    let mut previous_match = None;
    let mut score = 0i64;

    for needle in &query {
        let mut matched_index = None;

        for (index, candidate) in haystack.iter().enumerate().skip(next_index) {
            if candidate == needle {
                matched_index = Some(index);
                break;
            }
        }

        let index = matched_index?;
        score += 10;

        if index == 0 {
            score += 20;
        }

        if let Some(previous) = previous_match {
            if index == previous + 1 {
                score += 15;
            } else {
                score -= (index - previous - 1) as i64;
            }
        }

        if index == 0 || is_word_boundary(haystack[index - 1]) {
            score += 8;
        }

        previous_match = Some(index);
        next_index = index + 1;
    }

    if starts_with(&haystack, &query) {
        score += 35;
    }

    score -= (haystack.len().saturating_sub(query.len())) as i64 / 2;
    Some(score)
}

fn normalize(input: &str) -> Vec<char> {
    input.chars().flat_map(|ch| ch.to_lowercase()).collect()
}

fn is_word_boundary(ch: char) -> bool {
    matches!(ch, ' ' | '-' | '_' | '/' | '.' | ':')
}

fn starts_with(haystack: &[char], needle: &[char]) -> bool {
    haystack.len() >= needle.len() && haystack.iter().zip(needle.iter()).all(|(a, b)| a == b)
}
