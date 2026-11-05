//! The journal: one row per done check the hook ran, and the labels a
//! person gave them afterwards.
//!
//! The hook decides in the moment; whether its interventions were worth
//! their time is only known later, by the person. The journal keeps what
//! is needed to answer that: when a check ran, what it decided, which rule
//! fired, how long it took, and whether the person called the intervention
//! useful, noise or wrong. It lives beside the hook state, outside the
//! repository, one append-only file per repository.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

const SCHEMA: &str = "emi-hook-journal-0.1";

/// One check, as the hook saw it.
pub struct Check<'a> {
    pub session: &'a str,
    pub content_id: &'a str,
    /// `block` (the agent was sent back) or `notify` (it was let stop).
    pub reaction: &'a str,
    /// The done check's action: `stop`, `repair`, `ask_person`, `incomplete`.
    pub action: &'a str,
    /// Repair rounds used after this check, and the limit.
    pub round: u32,
    pub limit: u32,
    pub seconds: f64,
    /// What the proposer was told, when it was sent back.
    pub repair: &'a [String],
    /// What the receipt showed, in counts (see `receipt::evidence`).
    pub evidence: &'a Value,
}

/// The journal file of `repo` (already canonical) under `state_home`.
pub fn path(state_home: &Path, repo: &Path) -> PathBuf {
    let key = emi_kernel::hash::sha256_hex(repo.to_string_lossy().as_bytes());
    state_home
        .join("emi")
        .join("hook")
        .join(&key[..24])
        .join("journal.jsonl")
}

fn append(file: &Path, row: &Value) -> Result<(), String> {
    let fail = |what: &str, e: std::io::Error| format!("cannot {what} {}: {e}", file.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| fail("create the directory for", e))?;
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|e| fail("open", e))?;
    // one write per row, so two hooks appending at once cannot interleave
    out.write_all(format!("{row}\n").as_bytes())
        .map_err(|e| fail("write", e))
}

/// Record one check; returns the id a person labels it by.
pub fn record(state_home: &Path, repo: &Path, check: &Check) -> Result<String, String> {
    let at = emi_exec::clock::utc_now_iso();
    let seed = format!("{}\n{}\n{at}", check.session, check.content_id);
    let id = emi_kernel::hash::sha256_hex(seed.as_bytes())[..10].to_owned();
    let row = json!({
        "schema": SCHEMA, "kind": "check", "id": id, "at": at,
        "repo": repo.to_string_lossy(), "session": check.session,
        "content_id": check.content_id, "reaction": check.reaction,
        "action": check.action, "round": check.round, "limit": check.limit,
        "seconds": (check.seconds * 10.0).round() / 10.0,
        "repair": check.repair, "evidence": check.evidence,
    });
    append(&path(state_home, repo), &row)?;
    Ok(id)
}

/// Every row of the journal, oldest first. A missing file is an empty
/// journal; a line that cannot be read is an error, not a skipped row.
pub fn rows(state_home: &Path, repo: &Path) -> Result<Vec<Value>, String> {
    let file = path(state_home, repo);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| {
            serde_json::from_str::<Value>(l)
                .map_err(|e| format!("{} line {}: {e}", file.display(), n + 1))
        })
        .collect()
}

fn checks(rows: &[Value]) -> Vec<&Value> {
    rows.iter().filter(|r| r["kind"] == "check").collect()
}

/// The latest label of each check id.
fn labels(rows: &[Value]) -> BTreeMap<&str, &Value> {
    rows.iter()
        .filter(|r| r["kind"] == "label")
        .filter_map(|r| r["id"].as_str().map(|id| (id, r)))
        .collect()
}

/// A person's judgement of one intervention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Label {
    /// It caught something, or told me something I needed.
    Useful,
    /// It was right but not worth the time.
    Noise,
    /// It was mistaken.
    Wrong,
}

impl Label {
    fn as_str(self) -> &'static str {
        match self {
            Label::Useful => "useful",
            Label::Noise => "noise",
            Label::Wrong => "wrong",
        }
    }
}

/// Label the check whose id starts with `prefix`.
pub fn label(
    state_home: &Path,
    repo: &Path,
    prefix: &str,
    label: Label,
    note: Option<&str>,
) -> Result<String, String> {
    let all = rows(state_home, repo)?;
    let matches: Vec<&str> = checks(&all)
        .iter()
        .filter_map(|r| r["id"].as_str())
        .filter(|id| id.starts_with(prefix))
        .collect();
    let id = match matches.as_slice() {
        [one] => (*one).to_owned(),
        [] => {
            return Err(format!(
                "no check in the journal has an id starting with {prefix}"
            ));
        }
        many => {
            return Err(format!(
                "{prefix} matches {} checks; give more of the id",
                many.len()
            ));
        }
    };
    let row = json!({"schema": SCHEMA, "kind": "label", "id": id,
        "at": emi_exec::clock::utc_now_iso(), "label": label.as_str(), "note": note});
    append(&path(state_home, repo), &row)?;
    Ok(id)
}

fn tally<'a>(items: impl Iterator<Item = &'a str>) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for item in items {
        *counts.entry(item).or_default() += 1;
    }
    let mut out: Vec<(String, usize)> =
        counts.into_iter().map(|(k, n)| (k.to_owned(), n)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

/// A check whose action was not `stop`: the hook said something a person
/// or the proposer had to act on.
fn is_intervention(check: &Value) -> bool {
    check["action"] != "stop"
}

/// The journal in numbers.
pub fn summary(rows: &[Value]) -> Value {
    let all = checks(rows);
    let given = labels(rows);
    let seconds: Vec<f64> = all.iter().filter_map(|c| c["seconds"].as_f64()).collect();
    let interventions: Vec<&&Value> = all.iter().filter(|c| is_intervention(c)).collect();
    let labelled = interventions
        .iter()
        .filter_map(|c| c["id"].as_str().and_then(|id| given.get(id)))
        .filter_map(|l| l["label"].as_str());
    let unlabelled: Vec<Value> = interventions
        .iter()
        .filter(|c| c["id"].as_str().is_none_or(|id| !given.contains_key(id)))
        .map(|c| {
            json!({"id": c["id"], "at": c["at"], "action": c["action"],
                   "rule": c["evidence"]["kind"], "said": c["repair"][0]})
        })
        .collect();
    let sessions: std::collections::BTreeSet<&str> =
        all.iter().filter_map(|c| c["session"].as_str()).collect();
    json!({
        "checks": all.len(),
        "sessions": sessions.len(),
        "first": all.first().map(|c| c["at"].clone()),
        "last": all.last().map(|c| c["at"].clone()),
        "actions": tally(all.iter().filter_map(|c| c["action"].as_str())),
        "rules": tally(interventions.iter().filter_map(|c| c["evidence"]["kind"].as_str())),
        "sent_back": all.iter().filter(|c| c["reaction"] == "block").count(),
        "seconds_median": median(seconds.clone()),
        "seconds_longest": seconds.iter().copied().fold(None, |m: Option<f64>, s| Some(m.map_or(s, |m| m.max(s)))),
        "seconds_total": seconds.iter().sum::<f64>(),
        "interventions": interventions.len(),
        "labels": tally(labelled),
        "unlabelled": unlabelled,
    })
}

fn pairs(v: &Value) -> String {
    let items: Vec<String> = v
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| format!("{} {}", p[0].as_str().unwrap_or("?"), p[1]))
        .collect();
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

/// The summary in plain words.
pub fn text(repo: &Path, s: &Value) -> String {
    let secs = |key: &str| {
        s[key]
            .as_f64()
            .map_or("-".to_owned(), |v| format!("{v:.0} s"))
    };
    let mut lines = vec![
        format!(
            "EMI journal for {}: {} checks in {} sessions.",
            repo.display(),
            s["checks"],
            s["sessions"]
        ),
        format!("Actions: {}.", pairs(&s["actions"])),
        format!(
            "Interventions: {} (the agent was sent back {} times). By rule: {}.",
            s["interventions"],
            s["sent_back"],
            pairs(&s["rules"])
        ),
        format!(
            "Time per check: median {}, longest {}, total {}.",
            secs("seconds_median"),
            secs("seconds_longest"),
            secs("seconds_total")
        ),
        format!("Labels: {}.", pairs(&s["labels"])),
    ];
    let open = s["unlabelled"].as_array().map_or(&[][..], Vec::as_slice);
    if !open.is_empty() {
        lines.push("Not labelled yet (emi journal label ID useful|noise|wrong):".to_owned());
    }
    for c in open {
        lines.push(format!(
            "  {}  {}  {}  {}  {}",
            c["id"].as_str().unwrap_or("?"),
            c["at"].as_str().unwrap_or("?"),
            c["action"].as_str().unwrap_or("?"),
            c["rule"].as_str().unwrap_or("-"),
            c["said"].as_str().unwrap_or("")
        ));
    }
    lines.join("\n") + "\n"
}

#[derive(Subcommand)]
pub enum Command {
    /// What the hook did in this repository, in numbers, and which
    /// interventions are not labelled yet.
    Report(ReportArgs),
    /// Say whether one intervention was useful, noise or wrong.
    Label(LabelArgs),
}

#[derive(Args)]
pub struct ReportArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    format: String,
}

#[derive(Args)]
pub struct LabelArgs {
    /// The check's id, or enough of its start to name one check.
    id: String,
    #[arg(value_enum)]
    label: Label,
    /// Why, in a few words.
    #[arg(long)]
    note: Option<String>,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

fn located(repo: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    let state_home = crate::state::state_home().map_err(anyhow::Error::msg)?;
    let root = crate::repo::locate(repo)
        .map_err(anyhow::Error::msg)?
        .ok_or_else(|| anyhow::anyhow!("{} has no .emi/config.json", repo.display()))?;
    Ok((state_home, root))
}

pub fn run(command: &Command) -> anyhow::Result<u8> {
    let mut out = std::io::stdout().lock();
    match command {
        Command::Report(a) => {
            let (state_home, root) = located(&a.repo)?;
            let s = summary(&rows(&state_home, &root).map_err(anyhow::Error::msg)?);
            if a.format == "json" {
                writeln!(out, "{s}")?;
            } else {
                write!(out, "{}", text(&root, &s))?;
            }
        }
        Command::Label(a) => {
            let (state_home, root) = located(&a.repo)?;
            let id = label(&state_home, &root, &a.id, a.label, a.note.as_deref())
                .map_err(anyhow::Error::msg)?;
            writeln!(out, "{id} labelled {}", a.label.as_str())?;
        }
    }
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn check<'a>(
        session: &'a str,
        content: &'a str,
        action: &'a str,
        evidence: &'a Value,
    ) -> Check<'a> {
        Check {
            session,
            content_id: content,
            reaction: if action == "repair" {
                "block"
            } else {
                "notify"
            },
            action,
            round: u32::from(action == "repair"),
            limit: 3,
            seconds: 61.26,
            repair: &[],
            evidence,
        }
    }

    #[test]
    fn checks_are_recorded_summarised_and_labelled() {
        let home = tempfile::tempdir().expect("state");
        let repo = Path::new("/r");
        let redundant = json!({"kind": "redundant_test"});
        let none = json!({"kind": "none"});
        let first =
            record(home.path(), repo, &check("s1", "c1", "repair", &redundant)).expect("record");
        record(home.path(), repo, &check("s1", "c2", "stop", &none)).expect("record");
        record(
            home.path(),
            repo,
            &check(
                "s2",
                "c3",
                "ask_person",
                &json!({"kind": "changed_old_test"}),
            ),
        )
        .expect("record");

        let all = rows(home.path(), repo).expect("rows");
        let s = summary(&all);
        assert_eq!(
            (s["checks"].clone(), s["sessions"].clone()),
            (json!(3), json!(2))
        );
        assert_eq!(s["interventions"], 2);
        assert_eq!(s["sent_back"], 1);
        assert_eq!(s["seconds_median"], 61.3);
        assert_eq!(s["unlabelled"].as_array().map(Vec::len), Some(2));

        let id = label(
            home.path(),
            repo,
            &first[..6],
            Label::Useful,
            Some("caught a vacuous test"),
        )
        .expect("label");
        assert_eq!(id, first);
        let s = summary(&rows(home.path(), repo).expect("rows"));
        assert_eq!(s["labels"], json!([["useful", 1]]));
        assert_eq!(s["unlabelled"].as_array().map(Vec::len), Some(1));
        let shown = text(repo, &s);
        assert!(shown.contains("3 checks in 2 sessions"), "{shown}");
        assert!(shown.contains("Not labelled yet"), "{shown}");
    }

    #[test]
    fn a_label_needs_one_matching_check() {
        let home = tempfile::tempdir().expect("state");
        let repo = Path::new("/r");
        assert!(label(home.path(), repo, "abc", Label::Noise, None).is_err());
        assert_eq!(rows(home.path(), repo).expect("rows"), Vec::<Value>::new());
    }

    #[test]
    fn a_line_that_cannot_be_read_is_an_error_not_a_skipped_row() {
        let home = tempfile::tempdir().expect("state");
        let repo = Path::new("/r");
        let file = path(home.path(), repo);
        std::fs::create_dir_all(file.parent().expect("dir")).expect("mkdir");
        std::fs::write(&file, "{not json\n").expect("write");
        assert!(rows(home.path(), repo).is_err());
    }
}
