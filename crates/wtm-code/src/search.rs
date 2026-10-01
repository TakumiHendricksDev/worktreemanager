//! Find in Files: every line in a set of files that matches a query.
//!
//! # In-process, over git's list, rather than `git grep`
//!
//! git decides *which* files — the list comes from `Git::files`, so `.gitignore` is still git's to
//! apply — and the matching happens here. Three things settled that:
//!
//! - **The runner cannot carry the answer.** It caps what it captures at 4 MiB and hands nothing
//!   back until the process exits. `git grep e` on a small repository prints more than that, so a
//!   one-letter query on a large one would be read, thrown away and cut off.
//! - **Highlighting needs every match's range.** `git grep --column` reports only the first match's
//!   column, so the frontend would have to find the rest again with a second regex engine whose
//!   dialect differs from git's in exactly the cases people use a regex for.
//! - **A new query should stop the old one at once.** Here that is a flag checked between files.
//!
//! The `regex` crate is also linear-time, so no query the user types — or pastes — can hang the
//! search the way a backtracking engine can.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use regex::bytes::{Regex, RegexBuilder};
use wtm_core::ports::exec::CancelToken;

use crate::{CodeError, Mask, resolve};

/// The most matches one search reports. Past this it says it stopped rather than keep going.
///
/// A list of two thousand lines is already more than anybody reads; a query that produces more is
/// one to narrow, and the count stopping here is the prompt to narrow it.
pub const MAX_MATCHES: usize = 2_000;

/// The most matching lines reported from one file, so one generated file cannot fill the list.
pub const MAX_LINES_PER_FILE: usize = 100;

/// A file larger than this is skipped. Source is almost never this big; bundles and dumps are.
pub const MAX_SEARCH_BYTES: u64 = 1024 * 1024;

/// How much of a long line a result shows, so a minified file's one line is not sent whole.
const LINE_WINDOW: usize = 400;

/// How the query is read, from the popup's toggles.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub mask: String,
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    /// 1-based.
    pub line: u32,
    /// The line, or a window of it around the first match when the line is long.
    pub text: String,
    /// Where `text` starts in the line, in UTF-16 units — nonzero only for a windowed long line.
    pub offset: u32,
    /// Each match on the line as `[start, end)` in UTF-16 units into `text`, which is how the
    /// frontend's strings and `CodeMirror`'s positions both count.
    pub ranges: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub hits: Vec<Hit>,
    /// Matches found, which is more than `hits` when several are on one line.
    pub matches: usize,
    /// Files with at least one match.
    pub files: usize,
    /// It stopped at a cap, so there are more matches than these.
    pub truncated: bool,
}

/// Search `files` under `root` for `query`.
///
/// Files are read by a few threads pulling the next path from a shared counter, in order. When
/// the match count reaches the cap they stop taking new files, so every file before the one that
/// crossed it has been read — which is what makes the reported matches the *first* ones in path
/// order, the same every time, rather than whichever thread happened to be fast.
///
/// A cancelled search returns `Err(Cancelled)`; its partial answer is for nobody.
pub fn search(
    root: &Path,
    files: &[String],
    query: &str,
    options: &SearchOptions,
    cancel: &CancelToken,
) -> Result<SearchResults, CodeError> {
    let matcher = Matcher::new(query, options)?;
    let mask = Mask::parse(&options.mask)?;
    let files: Vec<&String> = files.iter().filter(|path| mask.admits(path)).collect();

    let next = AtomicUsize::new(0);
    let found = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let mut per_file: Vec<(usize, Vec<Hit>, usize)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        if cancel.is_cancelled() || found.load(Ordering::Relaxed) >= MAX_MATCHES {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = files.get(index) else { break };
                        let Some(bytes) = readable(root, path) else {
                            continue;
                        };
                        let (hits, count) = matcher.lines(path, &bytes);
                        if count > 0 {
                            found.fetch_add(count, Ordering::Relaxed);
                            mine.push((index, hits, count));
                        }
                    }
                    mine
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    });
    if cancel.is_cancelled() {
        return Err(CodeError::Cancelled);
    }

    per_file.sort_by_key(|(index, _, _)| *index);
    let mut results = SearchResults::default();
    for (_, hits, count) in per_file {
        if results.matches >= MAX_MATCHES {
            results.truncated = true;
            break;
        }
        results.files += 1;
        results.matches += count;
        results.hits.extend(hits);
    }
    results.truncated |= found.load(Ordering::Relaxed) >= MAX_MATCHES;
    Ok(results)
}

/// A file's bytes if it is worth searching: a regular file, not too large, and not binary.
fn readable(root: &Path, rel: &str) -> Option<Vec<u8>> {
    let path = resolve(root, rel).ok()?;
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_SEARCH_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let binary = bytes.iter().take(8 * 1024).any(|b| *b == 0);
    (!binary).then_some(bytes)
}

struct Matcher {
    regex: Regex,
    whole_word: bool,
}

impl Matcher {
    fn new(query: &str, options: &SearchOptions) -> Result<Self, CodeError> {
        if query.is_empty() {
            return Err(CodeError::BadQuery("nothing to find".to_owned()));
        }
        let pattern = if options.regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(!options.case_sensitive)
            .multi_line(true)
            .build()
            .map_err(|e| CodeError::BadQuery(e.to_string()))?;
        Ok(Self {
            regex,
            whole_word: options.whole_word,
        })
    }

    /// The matching lines of one file, and how many matches they hold.
    fn lines(&self, path: &str, bytes: &[u8]) -> (Vec<Hit>, usize) {
        let mut hits: Vec<Hit> = Vec::new();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut count = 0;
        let mut line = 1_u32;
        let mut counted_to = 0;
        let mut line_start = 0;
        let mut current: Option<u32> = None;

        for found in self.regex.find_iter(bytes) {
            let (start, end) = (found.start(), found.end());
            // An empty match (`^`, `\b`) finds a position, not text. Nothing to show for it.
            if start == end || (self.whole_word && !whole_word(bytes, start, end)) {
                continue;
            }
            line += count_newlines(&bytes[counted_to..start]);
            counted_to = start;
            if current != Some(line) {
                if let Some(done) = current {
                    hits.push(hit(path, done, bytes, line_start, &spans));
                    if hits.len() >= MAX_LINES_PER_FILE {
                        spans.clear();
                        current = None;
                        break;
                    }
                }
                spans.clear();
                line_start = bytes[..start]
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map_or(0, |i| i + 1);
                current = Some(line);
            }
            // A match that runs past its line is shown up to the line's end.
            let line_end = bytes[start..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |i| start + i);
            spans.push((start, end.min(line_end)));
            count += 1;
        }
        if let Some(done) = current {
            hits.push(hit(path, done, bytes, line_start, &spans));
        }
        (hits, count)
    }
}

/// Build one line's result from its matches' byte spans.
fn hit(path: &str, line: u32, bytes: &[u8], line_start: usize, spans: &[(usize, usize)]) -> Hit {
    let line_end = bytes[line_start..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(bytes.len(), |i| line_start + i);
    let mut end_of_line = line_end;
    if end_of_line > line_start && bytes[end_of_line - 1] == b'\r' {
        end_of_line -= 1;
    }

    // A long line is shown as a window that starts a little before its first match.
    let first = spans.first().map_or(line_start, |span| span.0);
    let mut from = line_start;
    let mut to = end_of_line;
    if to - from > LINE_WINDOW {
        from = first.saturating_sub(80).max(line_start);
        while from > line_start && is_continuation(bytes[from]) {
            from -= 1;
        }
        to = (from + LINE_WINDOW).min(end_of_line);
        while to < end_of_line && is_continuation(bytes[to]) {
            to += 1;
        }
    }

    let units = |a: usize, b: usize| utf16_len(&bytes[a..b]);
    Hit {
        path: path.to_owned(),
        line,
        text: String::from_utf8_lossy(&bytes[from..to]).into_owned(),
        offset: units(line_start, from),
        ranges: spans
            .iter()
            .filter(|(s, _)| *s >= from && *s < to.max(from + 1))
            .map(|&(s, e)| (units(from, s), units(from, e.min(to))))
            .collect(),
    }
}

/// W: a match whose first or last character is part of a word must not continue one.
///
/// Checked on the edges rather than by wrapping the pattern in `\b…\b`, which is wrong for a query
/// that starts or ends with punctuation: `\b.foo(` needs a word character before the dot, so it
/// would miss `x.foo(` — exactly the call being looked for.
fn whole_word(bytes: &[u8], start: usize, end: usize) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    let before = start.checked_sub(1).map(|i| bytes[i]);
    let after = bytes.get(end).copied();
    let opens = !word(bytes[start]) || before.is_none_or(|b| !word(b));
    let closes = !word(bytes[end - 1]) || after.is_none_or(|b| !word(b));
    opens && closes
}

// The lint's suggestion is the `bytecount` crate, which this workspace does not carry, for a count
// that only ever runs over the stretch between one match and the next.
#[allow(clippy::naive_bytecount)]
fn count_newlines(bytes: &[u8]) -> u32 {
    u32::try_from(bytes.iter().filter(|b| **b == b'\n').count()).unwrap_or(u32::MAX)
}

fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// How many UTF-16 units `bytes` decode to, counting what lossy decoding would produce.
fn utf16_len(bytes: &[u8]) -> u32 {
    let units = String::from_utf8_lossy(bytes).encode_utf16().count();
    u32::try_from(units).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn worktree(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        (dir, files.iter().map(|(p, _)| (*p).to_owned()).collect())
    }

    fn find(files: &[(&str, &str)], query: &str, options: &SearchOptions) -> SearchResults {
        let (dir, paths) = worktree(files);
        search(dir.path(), &paths, query, options, &CancelToken::new()).unwrap()
    }

    fn opts() -> SearchOptions {
        SearchOptions::default()
    }

    #[test]
    fn a_literal_query_finds_every_line_with_its_ranges_and_counts() {
        let results = find(
            &[
                ("a.py", "event_list = 1\nprint(event_list, event_list)\n"),
                ("b.py", "nothing here\n"),
            ],
            "event_list",
            &opts(),
        );

        assert_eq!(results.matches, 3);
        assert_eq!(results.files, 1);
        assert_eq!(results.hits.len(), 2, "one hit per line");
        assert_eq!(results.hits[0].line, 1);
        assert_eq!(results.hits[0].ranges, [(0, 10)]);
        assert_eq!(results.hits[1].line, 2);
        assert_eq!(results.hits[1].ranges, [(6, 16), (18, 28)]);
        assert!(!results.truncated);
    }

    #[test]
    fn case_is_ignored_unless_asked_for() {
        let files = [("a.txt", "Event\nevent\n")];
        assert_eq!(find(&files, "event", &opts()).matches, 2);
        let exact = SearchOptions {
            case_sensitive: true,
            ..opts()
        };
        assert_eq!(find(&files, "event", &exact).hits[0].line, 2);
    }

    #[test]
    fn a_literal_query_treats_regex_characters_as_text() {
        let results = find(&[("a.js", "a.b\naxb\n")], "a.b", &opts());
        assert_eq!(results.matches, 1);
    }

    #[test]
    fn a_regex_query_is_a_regex_and_a_bad_one_says_why() {
        let regex = SearchOptions {
            regex: true,
            ..opts()
        };
        assert_eq!(
            find(&[("a.py", "def a():\ndef bb():\n")], r"def \w+\(", &regex).matches,
            2
        );

        let (dir, paths) = worktree(&[("a.py", "x\n")]);
        let err = search(dir.path(), &paths, "(", &regex, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, CodeError::BadQuery(_)), "{err:?}");
    }

    #[test]
    fn whole_word_checks_the_edges_so_punctuation_queries_still_match() {
        let words = SearchOptions {
            whole_word: true,
            ..opts()
        };
        let files = [("a.py", "event\nevent_list\nprevent\nself.event(\n")];

        let plain = find(&files, "event", &words);
        assert_eq!(
            plain.hits.iter().map(|h| h.line).collect::<Vec<_>>(),
            [1, 4],
            "not inside event_list or prevent"
        );
        // `\b.event\(` would need a word character before the dot and find nothing.
        assert_eq!(find(&files, ".event(", &words).hits[0].line, 4);
    }

    #[test]
    fn ranges_are_utf16_units_so_they_line_up_after_non_ascii_text() {
        // `é` is two bytes and one UTF-16 unit; `𝒳` is four bytes and two units.
        let results = find(&[("a.md", "café 𝒳 target\n")], "target", &opts());
        assert_eq!(results.hits[0].ranges, [(8, 14)]);
        assert_eq!(results.hits[0].text.encode_utf16().count(), 14);
    }

    #[test]
    fn the_mask_limits_which_files_are_read() {
        let files = [
            ("a.py", "needle\n"),
            ("b.js", "needle\n"),
            ("c.min.js", "needle\n"),
        ];
        let mask = SearchOptions {
            mask: "*.js, !*.min.js".to_owned(),
            ..opts()
        };
        let results = find(&files, "needle", &mask);
        assert_eq!(
            results
                .hits
                .iter()
                .map(|h| h.path.as_str())
                .collect::<Vec<_>>(),
            ["b.js"]
        );
    }

    #[test]
    fn binary_and_oversized_files_are_skipped() {
        let big = "needle\n".repeat(usize::try_from(MAX_SEARCH_BYTES).unwrap() / 7 + 1);
        let results = find(
            &[
                ("logo.png", "needle\0\0"),
                ("dump.sql", &big),
                ("ok.txt", "needle\n"),
            ],
            "needle",
            &opts(),
        );
        assert_eq!(results.files, 1);
        assert_eq!(results.hits[0].path, "ok.txt");
    }

    #[test]
    fn a_long_line_comes_back_as_a_window_around_the_match() {
        let line = format!("{}needle{}", "x".repeat(5_000), "y".repeat(5_000));
        let results = find(&[("bundle.js", &line)], "needle", &opts());
        let hit = &results.hits[0];

        assert!(hit.text.len() <= LINE_WINDOW + 4);
        assert_eq!(hit.offset, 5_000 - 80);
        assert_eq!(hit.ranges, [(80, 86)]);
    }

    #[test]
    fn the_first_matches_in_path_order_are_kept_when_the_cap_is_reached() {
        let text = "needle\n".repeat(MAX_LINES_PER_FILE);
        let files: Vec<(String, String)> = (0..40)
            .map(|i| (format!("f{i:02}.txt"), text.clone()))
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();

        let results = find(&borrowed, "needle", &opts());

        assert!(results.truncated);
        assert_eq!(results.matches, MAX_MATCHES);
        assert_eq!(results.files, MAX_MATCHES / MAX_LINES_PER_FILE);
        // The first twenty files, in order — not whichever threads finished first.
        assert_eq!(results.hits.first().unwrap().path, "f00.txt");
        assert_eq!(results.hits.last().unwrap().path, "f19.txt");
    }

    #[test]
    fn one_file_reports_at_most_its_share_of_lines() {
        let text = "needle\n".repeat(MAX_LINES_PER_FILE * 3);
        let results = find(&[("gen.py", &text)], "needle", &opts());
        assert_eq!(results.hits.len(), MAX_LINES_PER_FILE);
    }

    #[test]
    fn a_cancelled_search_reports_nothing() {
        let (dir, paths) = worktree(&[("a.txt", "needle\n")]);
        let cancel = CancelToken::new();
        cancel.cancel();
        assert_eq!(
            search(dir.path(), &paths, "needle", &opts(), &cancel),
            Err(CodeError::Cancelled)
        );
    }

    #[test]
    fn crlf_line_endings_are_not_shown_as_part_of_the_line() {
        let results = find(
            &[("win.txt", "first needle\r\nsecond\r\n")],
            "needle",
            &opts(),
        );
        assert_eq!(results.hits[0].text, "first needle");
    }
}
