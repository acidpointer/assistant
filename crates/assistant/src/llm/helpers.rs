pub fn find_stop(source: &str, stops: &Vec<String>) -> Option<usize> {
    stops
        .iter()
        .filter(|stop| !stop.is_empty())
        .filter_map(|stop| source.find(stop))
        .min()
}

pub fn holdback_len(text: &str, stops: &Vec<String>, max_stop_len: usize) -> usize {
    if max_stop_len == 0 {
        return 0;
    }

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
            return suffix.len();
        }
    }

    0
}
