//! Verify an ordered patch set against pristine source without changing files.
//!
//! Later patches may amend earlier hunks. Reconstruct each touched file from
//! the pinned source, applying every patch in order, then compare the complete
//! result with the build copy. Missing, partial, or unrelated edits fail closed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use patch_apply::{Line, Patch};

/// Compare the complete composed postcondition, including overlapping patches.
///
/// A missing or changed destination returns `Ok(false)` and triggers a fresh copy.
///
/// # Errors
/// Returns diagnostics for unreadable inputs or malformed/unapplicable patches.
pub fn patches_match_new_side(
    patches_dir: &Path,
    source: &Path,
    destination: &Path,
) -> Result<bool, String> {
    let mut paths = fs::read_dir(patches_dir)
        .map_err(|e| format!("read patch directory {}: {e}", patches_dir.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("read patch directory entry: {e}"))?;
    paths.retain(|p| p.extension().is_some_and(|ext| ext == "patch"));
    paths.sort();
    let mut expected: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
    for path in paths {
        let text =
            fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let patches =
            Patch::from_multiple(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        if patches.is_empty() {
            return Err(format!("no file patches in {}", path.display()));
        }
        for patch in patches {
            let creates = patch.old.path.as_ref() == "/dev/null";
            let deletes = patch.new.path.as_ref() == "/dev/null";
            let target = if deletes {
                patch.old.path.as_ref()
            } else {
                patch.new.path.as_ref()
            };
            let (_, relative) = target
                .split_once('/')
                .ok_or_else(|| format!("invalid patch path {target}"))?;
            let relative = PathBuf::from(relative);
            if relative.as_os_str().is_empty()
                || relative
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(format!("invalid patch path {target}"));
            }
            if !expected.contains_key(&relative) {
                let original = match fs::read_to_string(source.join(&relative)) {
                    Ok(text) => Some(text),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(format!("read pristine {}: {e}", relative.display())),
                };
                expected.insert(relative.clone(), original);
            }
            let current = expected.get(&relative).and_then(Option::as_deref);
            if creates && current.is_some() {
                return Err(format!(
                    "patch creates an existing file {}",
                    relative.display()
                ));
            }
            if !creates && current.is_none() {
                return Err(format!(
                    "patch modifies an absent file {}",
                    relative.display()
                ));
            }
            let current = current.unwrap_or("");
            let composed = apply_patch(current, &patch)
                .map_err(|e| format!("apply {} to {}: {e}", path.display(), relative.display()))?;
            expected.insert(relative, if deletes { None } else { Some(composed) });
        }
    }
    for (relative, contents) in expected {
        match (contents, fs::read_to_string(destination.join(&relative))) {
            (Some(expected), Ok(actual)) if expected == actual => {}
            (None, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            (_, Err(e)) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("read destination {}: {e}", relative.display()))
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// Apply exact hunk contents while preserving untouched bytes and EOF newlines.
///
/// `patch_apply::apply` drops EOF newlines in edited tails and adds them to
/// untouched unterminated tails. Use its parser only: ordered context matching
/// here keeps the Rust and command-line engines byte-for-byte compatible.
///
/// # Errors
/// Returns a diagnostic if hunk contents/counts do not match the source.
pub fn apply_patch(current: &str, patch: &Patch<'_>) -> Result<String, String> {
    let physical: Vec<&str> = current.split_inclusive('\n').collect();
    let logical: Vec<&str> = current.lines().collect();
    let mut output = String::new();
    let mut cursor = 0;
    for hunk in &patch.hunks {
        let pattern: Vec<&str> = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                Line::Context(text) | Line::Remove(text) => Some(*text),
                Line::Add(_) | Line::EndOfFile(_) => None,
            })
            .collect();
        let old_count = usize::try_from(hunk.old_range.count).map_err(|e| e.to_string())?;
        let new_count = usize::try_from(hunk.new_range.count).map_err(|e| e.to_string())?;
        if pattern.len() != old_count {
            return Err("hunk old-line count does not match its contents".into());
        }
        let start = if old_count == 0 {
            hunk.old_range.start
        } else {
            hunk.old_range.start.saturating_sub(1)
        };
        let preferred = usize::try_from(start).map_err(|e| e.to_string())?;
        let matches = |position: usize| {
            position >= cursor
                && position
                    .checked_add(pattern.len())
                    .and_then(|end| logical.get(position..end))
                    == Some(pattern.as_slice())
        };
        let position = if matches(preferred) {
            Some(preferred)
        } else if pattern.is_empty() {
            None
        } else {
            (cursor..=logical.len().saturating_sub(pattern.len()))
                .find(|&position| matches(position))
        }
        .ok_or_else(|| "hunk context does not match the source".to_owned())?;
        for line in &physical[cursor..position] {
            output.push_str(line);
        }
        cursor = position;
        let mut emitted = 0;
        let mut previous = None;
        for line in &hunk.lines {
            match line {
                Line::Context(_) => {
                    output.push_str(physical[cursor]);
                    cursor += 1;
                    emitted += 1;
                    previous = Some(false);
                }
                Line::Remove(_) => {
                    cursor += 1;
                    previous = None;
                }
                Line::Add(text) => {
                    output.push_str(text);
                    output.push('\n');
                    emitted += 1;
                    previous = Some(true);
                }
                Line::EndOfFile(_) => {
                    if previous == Some(true) {
                        output.pop();
                    }
                    previous = None;
                }
            }
        }
        if emitted != new_count {
            return Err("hunk new-line count does not match its contents".into());
        }
    }
    for line in &physical[cursor..] {
        output.push_str(line);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::patches_match_new_side;
    use std::{fs, path::PathBuf};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove verification fixture");
        }
    }

    #[test]
    fn verifies_composed_overlaps_and_rejects_partial_or_unrelated_changes() {
        let root =
            std::env::temp_dir().join(format!("llama-patch-verification-{}", std::process::id()));
        let _fixture = Fixture(root.clone());
        for name in ["patches", "source", "destination"] {
            fs::create_dir_all(root.join(name)).expect("fixture directories");
        }
        let patches = root.join("patches");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::write(source.join("file"), "before\nvalue\nafter\n").expect("pristine source");
        fs::write(
            patches.join("0001.patch"),
            "--- a/file\n+++ b/file\n@@ -1,3 +1,3 @@\n before\n-value\n+first\n after\n",
        )
        .expect("first patch");
        fs::write(
            patches.join("0002.patch"),
            "--- a/file\n+++ b/file\n@@ -1,3 +1,4 @@\n before\n-first\n+second\n+extra\n after\n",
        )
        .expect("overlapping patch");
        assert!(
            !patches_match_new_side(&patches, &source, &destination).expect("missing destination")
        );
        fs::write(destination.join("file"), "before\nsecond\nextra\nafter\n")
            .expect("composed destination");
        assert!(patches_match_new_side(&patches, &source, &destination).expect("composed result"));
        for invalid in ["before\nfirst\nafter\n", "tampered\nsecond\nextra\nafter\n"] {
            fs::write(destination.join("file"), invalid).expect("invalid destination");
            assert!(!patches_match_new_side(&patches, &source, &destination)
                .expect("reject changed destination"));
        }
    }
    #[test]
    fn preserves_newlines_and_unterminated_tails_and_zero_context_insertions() {
        let cases = [
            ("before\nvalue\n", "@@ -1,2 +1,2 @@\n before\n-value\n+after\n", "before\nafter\n"),
            ("before\nvalue", "@@ -1,2 +1,2 @@\n before\n-value\n\\ No newline at end of file\n+after\n\\ No newline at end of file\n", "before\nafter"),
            ("before\ntail", "@@ -1 +1 @@\n-before\n+after\n", "after\ntail"),
            ("before\ntail\n", "@@ -1 +1 @@\n-before\n+after\n", "after\ntail\n"),
            ("before\n", "@@ -1,0 +2 @@\n+after\n", "before\nafter\n"),
            ("", "@@ -0,0 +1 @@\n+created\n", "created\n"),
            ("removed\n", "@@ -1 +0,0 @@\n-removed\n", ""),
        ];
        for (source, hunk, expected) in cases {
            let text = format!("--- a/file\n+++ b/file\n{hunk}");
            let parsed = patch_apply::Patch::from_multiple(&text).expect("parse newline fixture");
            assert_eq!(parsed.len(), 1);
            assert_eq!(
                super::apply_patch(source, &parsed[0]).expect("apply newline fixture"),
                expected
            );
        }
    }
}
