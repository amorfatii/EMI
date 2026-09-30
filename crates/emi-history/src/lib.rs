//! Behavioral analysis from git history (plan Phase 4): per-file churn,
//! authorship, age and AI attribution, change coupling, and the input to
//! hotspot ranking. History is mined once with rename tracking, so a file
//! keeps its history across moves.

pub mod hotspots;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use regex::Regex;
use serde::Serialize;

pub type Result<T> = std::result::Result<T, String>;

/// Who or what produced a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Human,
    /// Attributed to an AI agent (author, or a `Co-authored-by` trailer).
    Ai,
    /// An automation account (`[bot]`).
    Bot,
}

/// Default AI attribution pattern (case-insensitive), matched against the
/// author and every `Co-authored-by` trailer.
pub const DEFAULT_AI_PATTERN: &str = r"(?i)\b(claude|anthropic|copilot|codex|openai|chatgpt|gpt-[0-9]|gemini|cursor(agent)?|devin|aider|sweep|tabnine|codewhisperer|amazon q|jules|opencode)\b";

/// History window read by default: days before the head commit's time.
pub const DEFAULT_DAYS: u32 = 365;
/// Change coupling by default: at least this many shared commits...
pub const DEFAULT_MIN_SHARED: u32 = 5;
/// ...with at least this degree (percent of the pair's mean revisions)...
pub const DEFAULT_MIN_DEGREE: f64 = 30.0;
/// ...counting only commits that touch at most this many files.
pub const DEFAULT_MAX_CHANGESET: usize = 30;

#[derive(Clone, Debug)]
pub struct Commit {
    pub sha: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub origin: Origin,
    /// `(scope-relative current path, added, deleted)`; binary files count 0.
    pub files: Vec<(String, u64, u64)>,
}

/// What to mine.
#[derive(Clone, Debug)]
pub struct Query {
    pub repo: std::path::PathBuf,
    /// Scope directory inside the repository (`""` for the root).
    pub source: String,
    pub head: String,
    /// Only commits within this many days before `head`'s commit time.
    pub days: Option<u32>,
    pub ai_pattern: String,
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = emi_git::command(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {} failed: {e}", args.first().copied().unwrap_or("")))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr)
            .trim()
            .chars()
            .take(300)
            .collect::<String>();
        return Err(format!(
            "git {} failed: {detail}",
            args.first().copied().unwrap_or("")
        ));
    }
    Ok(out.stdout)
}

fn classify(author: &str, email: &str, trailers: &str, ai: &Regex) -> Origin {
    if author.ends_with("[bot]") || email.contains("[bot]") {
        return Origin::Bot;
    }
    let text = format!("{author} {email}\u{1d}{trailers}");
    if ai.is_match(&text) {
        Origin::Ai
    } else {
        Origin::Human
    }
}

/// Parse one numstat stream; renames arrive as `a\td\t\0old\0new\0`.
fn numstat(tokens: &[&str]) -> Vec<(String, Option<String>, u64, u64)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i].trim_start_matches('\n');
        i += 1;
        let mut fields = token.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let (a, d) = (a.parse().unwrap_or(0), d.parse().unwrap_or(0));
        if path.is_empty() {
            if let (Some(old), Some(new)) = (tokens.get(i), tokens.get(i + 1)) {
                out.push(((*new).to_owned(), Some((*old).to_owned()), a, d));
            }
            i += 2;
        } else {
            out.push((path.to_owned(), None, a, d));
        }
    }
    out
}

fn head_time(repo: &Path, head: &str) -> Result<i64> {
    let out = git(
        repo,
        &["log", "-1", "--format=%ct", "--end-of-options", head],
    )?;
    String::from_utf8_lossy(&out)
        .trim()
        .parse()
        .map_err(|_| format!("cannot read the commit time of {head}"))
}

/// Mine the history of the scope, newest first, with paths expressed as
/// the scope-relative path each file has at `head`.
pub fn mine(q: &Query) -> Result<Vec<Commit>> {
    let ai = Regex::new(&q.ai_pattern).map_err(|e| format!("invalid --ai-pattern: {e}"))?;
    // the head is resolved in the repository, the history read from a
    // clean view of its objects: its attributes (a `-diff` that hides line
    // counts) and drivers do not apply
    let head = git(
        &q.repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{}^{{commit}}", q.head),
        ],
    )?;
    let head = String::from_utf8_lossy(&head).trim().to_owned();
    let view = emi_git::CleanView::of(&q.repo)?;
    let repo = view.path();
    let prefix = if q.source.is_empty() || q.source == "." {
        String::new()
    } else {
        format!("{}/", q.source.trim_end_matches('/'))
    };
    let mut args = vec![
        "log".to_owned(),
        "--no-textconv".into(),
        "--no-ext-diff".into(),
        "-M".into(),
        "--no-merges".into(),
        "-z".into(),
        "--numstat".into(),
        "--format=%x1e%H%x1f%an%x1f%ae%x1f%ct%x1f%(trailers:key=Co-authored-by,valueonly,separator=%x1d)".into(),
    ];
    if let Some(days) = q.days {
        let since = head_time(repo, &head)? - i64::from(days) * 86_400;
        args.push(format!("--since={since}"));
    }
    args.extend([
        "--end-of-options".into(),
        head.clone(),
        "--".into(),
        if prefix.is_empty() {
            ".".into()
        } else {
            prefix.clone()
        },
    ]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let raw = git(repo, &refs)?;
    let text = String::from_utf8_lossy(&raw);
    let mut renamed: HashMap<String, String> = HashMap::new();
    let mut commits = Vec::new();
    for record in text.split('\u{1e}').filter(|r| !r.trim().is_empty()) {
        let tokens: Vec<&str> = record.split('\0').collect();
        let header: Vec<&str> = tokens
            .first()
            .copied()
            .unwrap_or("")
            .split('\u{1f}')
            .collect();
        let [sha, author, email, time, trailers] = header.as_slice() else {
            continue;
        };
        let mut files = Vec::new();
        for (path, old, a, d) in numstat(&tokens[1..]) {
            let current = renamed.get(&path).cloned().unwrap_or_else(|| path.clone());
            if let Some(old) = old {
                renamed.insert(old, current.clone());
            }
            if let Some(rel) = current.strip_prefix(&prefix) {
                files.push((rel.to_owned(), a, d));
            }
        }
        commits.push(Commit {
            sha: (*sha).to_owned(),
            author: (*author).to_owned(),
            email: (*email).to_owned(),
            time: time.parse().unwrap_or(0),
            origin: classify(author, email, trailers, &ai),
            files,
        });
    }
    Ok(commits)
}

/// Per-file behavior over the mined commits.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FileHistory {
    pub revisions: u32,
    pub added: u64,
    pub deleted: u64,
    pub authors: u32,
    /// Share of revisions by the most frequent author (knowledge
    /// concentration; 1.0 is a single-owner file).
    pub main_author_share: f64,
    pub first_change: i64,
    pub last_change: i64,
    pub ai_revisions: u32,
    pub ai_churn: u64,
    pub bot_revisions: u32,
}

pub fn file_histories(commits: &[Commit]) -> BTreeMap<String, FileHistory> {
    let mut authors: HashMap<&str, HashMap<&str, u32>> = HashMap::new();
    let mut out: BTreeMap<String, FileHistory> = BTreeMap::new();
    for c in commits {
        for (path, a, d) in &c.files {
            let h = out.entry(path.clone()).or_default();
            h.revisions += 1;
            h.added += a;
            h.deleted += d;
            h.first_change = if h.first_change == 0 {
                c.time
            } else {
                h.first_change.min(c.time)
            };
            h.last_change = h.last_change.max(c.time);
            match c.origin {
                Origin::Ai => {
                    h.ai_revisions += 1;
                    h.ai_churn += a + d;
                }
                Origin::Bot => h.bot_revisions += 1,
                Origin::Human => {}
            }
            *authors
                .entry(path)
                .or_default()
                .entry(&c.email)
                .or_default() += 1;
        }
    }
    for (path, h) in &mut out {
        let counts = &authors[path.as_str()];
        h.authors = u32::try_from(counts.len()).unwrap_or(u32::MAX);
        let top = counts.values().copied().max().unwrap_or(0);
        h.main_author_share = f64::from(top) / f64::from(h.revisions.max(1));
    }
    out
}

/// Two files that change together.
#[derive(Clone, Debug, Serialize)]
pub struct Coupling {
    pub a: String,
    pub b: String,
    pub shared: u32,
    /// `shared / mean(revisions)`, as percent.
    pub degree: f64,
}

/// Temporal coupling: pairs co-changing in at least `min_shared` commits
/// with degree ≥ `min_degree` percent; commits touching more than
/// `max_changeset` scope files are ignored (mass reformatting).
pub fn change_coupling(
    commits: &[Commit],
    min_shared: u32,
    min_degree: f64,
    max_changeset: usize,
) -> Vec<Coupling> {
    let histories = file_histories(commits);
    let mut shared: HashMap<(&str, &str), u32> = HashMap::new();
    for c in commits {
        let mut files: Vec<&str> = c.files.iter().map(|f| f.0.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        if files.len() < 2 || files.len() > max_changeset {
            continue;
        }
        for (i, a) in files.iter().enumerate() {
            for b in &files[i + 1..] {
                *shared.entry((a, b)).or_default() += 1;
            }
        }
    }
    let mut out: Vec<Coupling> = shared
        .into_iter()
        .filter(|(_, n)| *n >= min_shared)
        .map(|((a, b), n)| {
            let mean = f64::from(histories[a].revisions + histories[b].revisions) / 2.0;
            Coupling {
                a: a.to_owned(),
                b: b.to_owned(),
                shared: n,
                degree: 100.0 * f64::from(n) / mean,
            }
        })
        .filter(|c| c.degree >= min_degree)
        .collect();
    out.sort_by(|x, y| {
        y.shared
            .cmp(&x.shared)
            .then(y.degree.total_cmp(&x.degree))
            .then(x.a.cmp(&y.a))
            .then(x.b.cmp(&y.b))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_reads_renames_and_binaries() {
        let tokens = [
            "\n3\t1\tlib/a.js",
            "2\t0\t",
            "lib/old.js",
            "lib/new.js",
            "-\t-\timg.png",
            "",
        ];
        let parsed = numstat(&tokens);
        assert_eq!(parsed[0], ("lib/a.js".into(), None, 3, 1));
        assert_eq!(
            parsed[1],
            ("lib/new.js".into(), Some("lib/old.js".into()), 2, 0)
        );
        assert_eq!(parsed[2], ("img.png".into(), None, 0, 0));
    }

    #[test]
    fn classifies_ai_and_bots() {
        let Ok(ai) = Regex::new(DEFAULT_AI_PATTERN) else {
            panic!("pattern")
        };
        assert_eq!(classify("dependabot[bot]", "x@y", "", &ai), Origin::Bot);
        assert_eq!(
            classify("Jo", "jo@x", "Claude Opus 5.5 <noreply@anthropic.com>", &ai),
            Origin::Ai
        );
        assert_eq!(classify("Jo", "jo@x", "Alex <alex@x>", &ai), Origin::Human);
        assert_eq!(
            classify("Cursor Agent", "cursoragent@cursor.com", "", &ai),
            Origin::Ai
        );
    }
}
