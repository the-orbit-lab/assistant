use orbit_core::SourceReference;

use crate::discovery::DiscoveredFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchKind {
    CaseInsensitiveContent,
    ExactContent,
    Heading,
    Filename,
}

impl MatchKind {
    fn score(self) -> u32 {
        match self {
            MatchKind::CaseInsensitiveContent => 40,
            MatchKind::ExactContent => 60,
            MatchKind::Heading => 90,
            MatchKind::Filename => 100,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub source: SourceReference,
    pub excerpt: String,
    pub score: u32,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub limit: usize,
    pub max_excerpt_chars: usize,
    /// Maximum matching lines kept per file, before global ranking.
    pub max_matches_per_file: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 20,
            max_excerpt_chars: 240,
            max_matches_per_file: 5,
        }
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}…")
}

fn is_markdown(file: &DiscoveredFile) -> bool {
    file.relative_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

/// Deterministic, local, model-free search across discovered text files.
///
/// Ranks filename matches highest, then Markdown heading matches, then
/// case-sensitive content matches, then case-insensitive content matches.
/// Ties break on path and then line number so results are stable across
/// runs.
pub fn search_files(
    files: &[DiscoveredFile],
    query: &str,
    options: &SearchOptions,
) -> Vec<SearchResult> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();
    let mut results: Vec<(MatchKind, SearchResult)> = Vec::new();

    for file in files {
        let display_path = file.relative_path.clone();
        let filename = display_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if filename.to_lowercase().contains(&query_lower) {
            results.push((
                MatchKind::Filename,
                SearchResult {
                    source: SourceReference::whole_file(display_path.clone()),
                    excerpt: format!("filename matches `{query}`"),
                    score: MatchKind::Filename.score(),
                },
            ));
        }

        if !file.is_text {
            continue;
        }
        let Ok(bytes) = std::fs::read(&file.absolute_path) else {
            continue;
        };
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };

        let mut per_file_matches = 0usize;
        for (idx, line) in content.lines().enumerate() {
            if per_file_matches >= options.max_matches_per_file {
                break;
            }
            let line_number = idx + 1;
            let line_lower = line.to_lowercase();
            if !line_lower.contains(&query_lower) {
                continue;
            }

            let trimmed = line.trim_start();
            let kind = if is_markdown(file) && trimmed.starts_with('#') {
                MatchKind::Heading
            } else if line.contains(query) {
                MatchKind::ExactContent
            } else {
                MatchKind::CaseInsensitiveContent
            };

            let section = if kind == MatchKind::Heading {
                Some(trimmed.trim_start_matches('#').trim().to_string())
            } else {
                None
            };

            let mut source = SourceReference::lines(display_path.clone(), line_number, line_number);
            if let Some(section) = section {
                source = source.with_section(section);
            }

            results.push((
                kind,
                SearchResult {
                    source,
                    excerpt: truncate(line.trim(), options.max_excerpt_chars),
                    score: kind.score(),
                },
            ));
            per_file_matches += 1;
        }
    }

    results.sort_by(|(_, a), (_, b)| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.source.path.cmp(&b.source.path))
            .then_with(|| a.source.line_start.cmp(&b.source.line_start))
    });

    results
        .into_iter()
        .map(|(_, result)| result)
        .take(options.limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn file(root: &std::path::Path, relative: &str, content: &str) -> DiscoveredFile {
        let absolute = root.join(relative);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&absolute, content).unwrap();
        DiscoveredFile {
            relative_path: PathBuf::from(relative),
            absolute_path: absolute,
            size: content.len() as u64,
            is_text: true,
        }
    }

    #[test]
    fn ranks_filename_above_content_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![
            file(root, "watchdog.md", "no query text here"),
            file(root, "other.md", "mentions watchdog in passing"),
        ];
        let results = search_files(&files, "watchdog", &SearchOptions::default());
        assert_eq!(results[0].source.path, PathBuf::from("watchdog.md"));
    }

    #[test]
    fn ranks_heading_above_plain_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![file(
            root,
            "doc.md",
            "some intro\n## Watchdog Design\nbody mentions watchdog here too\n",
        )];
        let results = search_files(&files, "watchdog", &SearchOptions::default());
        assert_eq!(results[0].source.line_start, Some(2));
        assert_eq!(
            results[0].source.section.as_deref(),
            Some("Watchdog Design")
        );
    }

    #[test]
    fn preserves_line_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![file(
            root,
            "notes.txt",
            "line one\nline two target\nline three\n",
        )];
        let results = search_files(&files, "target", &SearchOptions::default());
        assert_eq!(results[0].source.line_start, Some(2));
        assert_eq!(results[0].source.line_end, Some(2));
    }

    #[test]
    fn is_deterministic_across_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![
            file(root, "a.md", "alpha beta alpha\n"),
            file(root, "b.md", "alpha gamma\n"),
        ];
        let first = search_files(&files, "alpha", &SearchOptions::default());
        let second = search_files(&files, "alpha", &SearchOptions::default());
        let first_paths: Vec<_> = first.iter().map(|r| r.source.path.clone()).collect();
        let second_paths: Vec<_> = second.iter().map(|r| r.source.path.clone()).collect();
        assert_eq!(first_paths, second_paths);
    }

    #[test]
    fn empty_query_returns_no_results() {
        let results = search_files(&[], "  ", &SearchOptions::default());
        assert!(results.is_empty());
    }
}
