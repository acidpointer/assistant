//! Literal stop matching and UTF-8-safe streaming holdback; no native state.
pub fn find_stop(source: &str, stops: &[String]) -> Option<usize> {
    stops
        .iter()
        .filter(|stop| !stop.is_empty())
        .filter_map(|stop| source.find(stop))
        .min()
}

pub fn holdback_len(text: &str, stops: &[String], max_stop_len: usize) -> usize {
    if max_stop_len == 0 {
        return 0;
    }

    let mut longest = 0;
    for (index, _) in text.char_indices().rev() {
        let suffix = &text[index..];

        // Longer suffixes cannot be a prefix of any stop.
        if suffix.len() > max_stop_len {
            break;
        }

        if stops
            .iter()
            .any(|stop| !stop.is_empty() && stop.starts_with(suffix))
        {
            longest = suffix.len();
        }
    }

    longest
}

#[cfg(test)]
mod tests {
    use super::{find_stop, holdback_len};

    #[test]
    fn holds_the_longest_overlapping_stop_prefix() {
        let stops = vec!["aaaa".to_owned(), "aab".to_owned()];
        assert_eq!(holdback_len("text aaa", &stops, 4), 3);
        assert_eq!(holdback_len("text aa", &stops, 4), 2);
        assert_eq!(holdback_len("text aax", &stops, 4), 0);
    }

    #[test]
    fn holds_unicode_prefixes_at_character_boundaries() {
        let stops = vec!["🙂🙂!".to_owned()];
        assert_eq!(holdback_len("text 🙂🙂", &stops, 9), 8);
        assert_eq!(holdback_len("text 🙂", &stops, 9), 4);
    }

    #[test]
    fn finds_the_earliest_stop_and_ignores_empty_stops() {
        let stops = vec![String::new(), "later".to_owned(), "first".to_owned()];
        assert_eq!(find_stop("text first later", &stops), Some(5));
        assert_eq!(find_stop("text", &stops), None);
        assert_eq!(holdback_len("text", &[], 0), 0);
    }
}
