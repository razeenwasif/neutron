//! Background Git and Neutrino (`ntr`) inspection for local and remote folders.
//!
//! # Threading
//!
//! **The UI thread never spawns `git`, `ntr`, or SSH.** Views read cached
//! [`FolderVcs`] snapshots via [`VcsService::peek`], while `shared_frame`
//! requests updates via [`VcsService::request_if_needed`] that run on a
//! dedicated background worker thread.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use neutron_core::NodeId;
use neutron_ui::file_list::VcsStatus;

const CACHE_TTL: Duration = Duration::from_secs(20);
const CHILD_REPO_CAP: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Pass,
    Fail,
    Running,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtrInfo {
    pub slug: String,
    pub ci: Option<(String, CiState)>,
    pub open_prs: usize,
    pub mirror_ok: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderVcs {
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub dirty_count: usize,
    pub entries: HashMap<String, VcsStatus>,
    pub ntr: Option<NtrInfo>,
}

struct ProbeReq {
    loc: NodeId,
    site: Option<neutron_remote::Site>,
}

pub struct VcsService {
    tx: Sender<ProbeReq>,
    rx: Receiver<(NodeId, Option<FolderVcs>)>,
    cache: HashMap<NodeId, (Instant, Option<FolderVcs>)>,
    pending: HashSet<NodeId>,
}

impl VcsService {
    pub fn spawn(wake: egui::Context) -> Self {
        let (tx, inbox) = crossbeam_channel::unbounded::<ProbeReq>();
        let (done_tx, rx) = crossbeam_channel::unbounded();
        let _ = std::thread::Builder::new()
            .name("neutron-vcs".into())
            .spawn(move || {
                while let Ok(req) = inbox.recv() {
                    let result = probe(&req.loc, req.site.as_ref());
                    if done_tx.send((req.loc, result)).is_err() {
                        return;
                    }
                    wake.request_repaint();
                }
            });
        Self {
            tx,
            rx,
            cache: HashMap::new(),
            pending: HashSet::new(),
        }
    }

    pub fn peek(&self, loc: &NodeId) -> Option<&FolderVcs> {
        self.cache.get(loc).and_then(|(_, v)| v.as_ref())
    }

    pub fn request_if_needed(&mut self, loc: &NodeId, site: Option<&neutron_remote::Site>) {
        if cfg!(test) {
            return;
        }
        let fresh = self
            .cache
            .get(loc)
            .is_some_and(|(at, _)| at.elapsed() < CACHE_TTL);
        if fresh || self.pending.contains(loc) {
            return;
        }
        let supported = match loc {
            NodeId::Path(_) => true,
            NodeId::Remote { .. } => site.is_some_and(|s| {
                s.protocol == neutron_remote::Protocol::Sftp
                    && loc
                        .as_remote()
                        .is_some_and(|(_, p)| neutron_remote::sftp::windows_path_from_sftp(p).is_none())
            }),
            _ => false,
        };
        if !supported {
            return;
        }
        self.pending.insert(loc.clone());
        let _ = self.tx.send(ProbeReq {
            loc: loc.clone(),
            site: site.cloned(),
        });
    }

    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok((loc, res)) = self.rx.try_recv() {
            self.pending.remove(&loc);
            self.cache.insert(loc, (Instant::now(), res));
            changed = true;
        }
        changed
    }

    pub fn invalidate(&mut self, loc: &NodeId) {
        self.cache.remove(loc);
    }
}

fn probe(loc: &NodeId, site: Option<&neutron_remote::Site>) -> Option<FolderVcs> {
    match loc {
        NodeId::Path(dir) => probe_local(dir),
        NodeId::Remote { path, .. } => probe_remote(site?, path),
        _ => None,
    }
}

fn probe_local(dir: &Path) -> Option<FolderVcs> {
    if !dir.is_dir() {
        return None;
    }
    let prefix_out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-prefix"])
        .output()
        .ok();
    if let Some(out) = prefix_out
        && out.status.success()
    {
        let rel_prefix = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let status_out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["status", "--porcelain=v2", "--branch"])
            .output()
            .ok()?;
        if !status_out.status.success() {
            return None;
        }
        let mut vcs = parse_git_porcelain_v2(&String::from_utf8_lossy(&status_out.stdout), &rel_prefix);
        if let Ok(remote_out) = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "--get", "remote.origin.url"])
            .output()
            && remote_out.status.success()
        {
            let url = String::from_utf8_lossy(&remote_out.stdout);
            if let Some(slug) = extract_ntr_slug(url.trim()) {
                vcs.ntr = probe_local_ntr(&slug);
            }
        }
        return Some(vcs);
    }

    // Parent directory fallback: mark immediate subdirectories that contain `.git`.
    let mut entries = HashMap::new();
    let rd = std::fs::read_dir(dir).ok()?;
    let mut checked = 0usize;
    for item in rd.flatten() {
        if checked >= CHILD_REPO_CAP {
            break;
        }
        let child = item.path();
        if !child.is_dir() || !child.join(".git").exists() {
            continue;
        }
        checked += 1;
        let name = item.file_name().to_string_lossy().into_owned();
        let dirty = Command::new("git")
            .arg("-C")
            .arg(&child)
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .is_some_and(|o| o.status.success() && !o.stdout.is_empty());
        entries.insert(name, if dirty { VcsStatus::RepoDirty } else { VcsStatus::RepoClean });
    }
    if entries.is_empty() {
        None
    } else {
        Some(FolderVcs {
            entries,
            ..Default::default()
        })
    }
}

pub(crate) fn find_ntr_bin() -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let p = std::path::PathBuf::from(home).join(".neutrino/bin/ntr");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn probe_local_ntr(slug: &str) -> Option<NtrInfo> {
    let ntr = find_ntr_bin()?;
    let runs_out = Command::new(&ntr)
        .args(["-r", slug, "runs"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let pr_out = Command::new(&ntr)
        .args(["-r", slug, "pr", "list"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mirror_out = Command::new(&ntr)
        .arg("mirror")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    Some(NtrInfo {
        slug: slug.to_owned(),
        ci: parse_ntr_runs(&runs_out),
        open_prs: parse_ntr_open_prs(&pr_out),
        mirror_ok: parse_ntr_mirror_ok(&mirror_out, slug),
    })
}

fn probe_remote(site: &neutron_remote::Site, remote_dir: &str) -> Option<FolderVcs> {
    let mut transport = crate::remote::connect_checked(site).ok()?;
    let norm = neutron_remote::path::normalize(remote_dir);
    let q_dir = neutron_remote::sftp::shell_quote_posix(&norm);
    let script = concat!(
        r#"d="$1"; "#,
        r#"if p=$(git -C "$d" rev-parse --show-prefix 2>/dev/null); then "#,
        r#"  printf "__P__\n%s\n__S__\n" "$p"; "#,
        r#"  git -C "$d" status --porcelain=v2 --branch 2>/dev/null; "#,
        r#"  printf "__R__\n"; "#,
        r#"  u=$(git -C "$d" config --get remote.origin.url 2>/dev/null); "#,
        r#"  printf "%s\n" "$u"; "#,
        r#"  ntr_bin="$HOME/.neutrino/bin/ntr"; [ -x "$ntr_bin" ] || ntr_bin=$(command -v ntr 2>/dev/null || true); "#,
        r#"  case "$u" in */.neutrino/repos/*) "#,
        r#"    if [ -n "$ntr_bin" ]; then "#,
        r#"      slug=$(printf "%s" "$u" | sed -E 's|.*/\.neutrino/repos/([^/]+)(\.git)?/?$|\1|' | tr '[:upper:]' '[:lower:]'); "#,
        r#"      printf "__N__\n%s\n__RUNS__\n" "$slug"; "#,
        r#"      "$ntr_bin" -r "$slug" runs 2>/dev/null | head -n 3; "#,
        r#"      printf "__PR__\n"; "#,
        r#"      "$ntr_bin" -r "$slug" pr list 2>/dev/null; "#,
        r#"      printf "__MIR__\n"; "#,
        r#"      "$ntr_bin" mirror 2>/dev/null; "#,
        r#"    fi ;; "#,
        r#"  esac; "#,
        r#"else "#,
        r#"  printf "__CHILDREN__\n"; "#,
        r#"  n=0; for c in "$d"/*/; do "#,
        r#"    [ -d "$c/.git" ] || continue; "#,
        r#"    n=$((n+1)); [ "$n" -le 40 ] || break; "#,
        r#"    bn=$(basename "$c"); "#,
        r#"    if [ -n "$(git -C "$c" status --porcelain 2>/dev/null | head -n 1)" ]; then printf "D\t%s\n" "$bn"; else printf "C\t%s\n" "$bn"; fi; "#,
        r#"  done; "#,
        r#"fi"#
    );
    let cmd = format!(
        "sh -c {} _ {}",
        neutron_remote::sftp::shell_quote_posix(script),
        q_dir
    );
    let stdout = transport.exec(&cmd, Duration::from_secs(6)).ok()?;
    parse_remote_vcs_output(&stdout)
}

pub(crate) fn parse_remote_vcs_output(stdout: &str) -> Option<FolderVcs> {
    if stdout.contains("__CHILDREN__") {
        let mut entries = HashMap::new();
        let after = stdout.split("__CHILDREN__").nth(1).unwrap_or("");
        for line in after.lines() {
            let Some((flag, name)) = line.trim_end_matches('\r').split_once('\t') else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let st = if flag == "D" {
                VcsStatus::RepoDirty
            } else {
                VcsStatus::RepoClean
            };
            entries.insert(name.to_owned(), st);
        }
        return (!entries.is_empty()).then_some(FolderVcs {
            entries,
            ..Default::default()
        });
    }

    let (_, after_p) = stdout.split_once("__P__\n")?;
    let (prefix_raw, after_s) = after_p.split_once("\n__S__\n")?;
    let (status_raw, after_r) = after_s.split_once("\n__R__\n")?;
    let rel_prefix = prefix_raw.trim();
    let mut vcs = parse_git_porcelain_v2(status_raw, rel_prefix);

    let (remote_url, rest) = if let Some((u, r)) = after_r.split_once("\n__N__\n") {
        (u.trim(), Some(r))
    } else {
        (after_r.trim(), None)
    };
    if let Some(slug) = extract_ntr_slug(remote_url)
        && let Some(ntr_sec) = rest
    {
        let (slug_line, after_runs) = ntr_sec.split_once("\n__RUNS__\n").unwrap_or((&slug, ""));
        let (runs_raw, after_pr) = after_runs.split_once("\n__PR__\n").unwrap_or((after_runs, ""));
        let (pr_raw, mirror_raw) = after_pr.split_once("\n__MIR__\n").unwrap_or((after_pr, ""));
        let s = slug_line.trim();
        let slug_use = if s.is_empty() { slug } else { s.to_owned() };
        vcs.ntr = Some(NtrInfo {
            ci: parse_ntr_runs(runs_raw),
            open_prs: parse_ntr_open_prs(pr_raw),
            mirror_ok: parse_ntr_mirror_ok(mirror_raw, &slug_use),
            slug: slug_use,
        });
    }
    Some(vcs)
}

fn rank_status(s: VcsStatus) -> u8 {
    match s {
        VcsStatus::Conflicted => 4,
        VcsStatus::Modified => 3,
        VcsStatus::Added => 2,
        VcsStatus::Untracked => 1,
        VcsStatus::RepoDirty | VcsStatus::RepoClean => 0,
    }
}

/// Parses `git status --porcelain=v2 --branch` output into [`FolderVcs`],
/// mapping changed paths relative to `rel_prefix` onto immediate children.
pub(crate) fn parse_git_porcelain_v2(stdout: &str, rel_prefix: &str) -> FolderVcs {
    let mut out = FolderVcs::default();
    let mut head_oid = None::<String>;
    let prefix = rel_prefix.trim_matches('/');

    let mut insert_path = |repo_path: &str, status: VcsStatus| {
        let p = repo_path.trim_matches('"');
        let rel = if prefix.is_empty() {
            p
        } else if let Some(rest) = p.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) {
            rest
        } else {
            return;
        };
        let child = rel.split('/').next().unwrap_or("");
        if child.is_empty() {
            return;
        }
        out.entries
            .entry(child.to_owned())
            .and_modify(|existing| {
                if rank_status(status) > rank_status(*existing) {
                    *existing = status;
                }
            })
            .or_insert(status);
    };

    for raw in stdout.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            let b = rest.trim();
            if b != "(detached)" {
                out.branch = Some(b.to_owned());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("# branch.oid ") {
            let oid = rest.trim();
            if oid != "(initial)" && oid.len() >= 7 {
                head_oid = Some(oid[..7].to_owned());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("# branch.ab ") {
            for tok in rest.split_whitespace() {
                if let Some(n) = tok.strip_prefix('+') {
                    out.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = tok.strip_prefix('-') {
                    out.behind = n.parse().unwrap_or(0);
                }
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        out.dirty_count += 1;

        if let Some(path) = line.strip_prefix("? ") {
            insert_path(path.trim(), VcsStatus::Untracked);
        } else if line.starts_with("1 ") {
            let mut parts = line.splitn(9, ' ');
            let _tag = parts.next();
            let xy = parts.next().unwrap_or("..");
            for _ in 0..6 {
                parts.next();
            }
            if let Some(path) = parts.next() {
                let st = if xy.contains('M') || xy.ends_with('D') {
                    VcsStatus::Modified
                } else if xy.starts_with('A') {
                    VcsStatus::Added
                } else {
                    VcsStatus::Modified
                };
                insert_path(path.trim(), st);
            }
        } else if line.starts_with("2 ") {
            let mut parts = line.splitn(10, ' ');
            for _ in 0..9 {
                parts.next();
            }
            if let Some(path_pair) = parts.next() {
                let new_path = path_pair.split('\t').next().unwrap_or(path_pair);
                insert_path(new_path.trim(), VcsStatus::Added);
            }
        } else if line.starts_with("u ") {
            let mut parts = line.splitn(11, ' ');
            for _ in 0..10 {
                parts.next();
            }
            if let Some(path) = parts.next() {
                insert_path(path.trim(), VcsStatus::Conflicted);
            }
        }
    }

    if out.branch.is_none() {
        out.branch = head_oid;
    }
    out
}

pub(crate) fn extract_ntr_slug(remote_url: &str) -> Option<String> {
    let (_, after) = remote_url.trim().split_once("/.neutrino/repos/")?;
    let clean = after
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(after.trim_end_matches('/'));
    if clean.is_empty() || clean.contains('/') {
        None
    } else {
        Some(clean.to_ascii_lowercase())
    }
}

pub(crate) fn parse_ntr_runs(stdout: &str) -> Option<(String, CiState)> {
    let first = stdout.lines().find(|l| !l.trim().is_empty())?;
    let mut toks = first.split_whitespace();
    let run_id = toks.next()?.to_owned();
    let status = toks.next()?.to_ascii_lowercase();
    let state = match status.as_str() {
        "success" | "pass" | "passed" | "ok" => CiState::Pass,
        "failure" | "failed" | "error" => CiState::Fail,
        "running" | "queued" | "pending" | "in_progress" => CiState::Running,
        _ => CiState::Unknown,
    };
    Some((run_id, state))
}

pub(crate) fn parse_ntr_open_prs(stdout: &str) -> usize {
    stdout
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && trimmed
                    .split_whitespace()
                    .nth(1)
                    .is_some_and(|s| s.eq_ignore_ascii_case("open"))
        })
        .count()
}

pub(crate) fn parse_ntr_mirror_ok(stdout: &str, slug: &str) -> Option<bool> {
    for line in stdout.lines() {
        let mut toks = line.split_whitespace();
        let Some(status) = toks.next() else { continue };
        let Some(repo) = toks.next() else { continue };
        if repo.eq_ignore_ascii_case(slug) {
            return Some(status.eq_ignore_ascii_case("ok"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v2_branch_counts_and_subdir_status() {
        let sample = "# branch.oid ff2d4340dec718e0\n# branch.head master\n# branch.upstream origin/master\n# branch.ab +2 -1\n1 .M N... 100644 100644 100644 aaa bbb crates/neutron-app/src/app.rs\n? assets/rail/copy.rgba\nu UU N... 100644 100644 100644 100644 aaa bbb ccc Conflict.txt\n";
        let root = parse_git_porcelain_v2(sample, "");
        assert_eq!(root.branch.as_deref(), Some("master"));
        assert_eq!(root.ahead, 2);
        assert_eq!(root.behind, 1);
        assert_eq!(root.dirty_count, 3);
        assert_eq!(root.entries.get("crates"), Some(&VcsStatus::Modified));
        assert_eq!(root.entries.get("assets"), Some(&VcsStatus::Untracked));
        assert_eq!(root.entries.get("Conflict.txt"), Some(&VcsStatus::Conflicted));

        let sub = parse_git_porcelain_v2(sample, "crates/neutron-app/src/");
        assert_eq!(sub.entries.get("app.rs"), Some(&VcsStatus::Modified));
        assert!(!sub.entries.contains_key("Conflict.txt"));
    }

    #[test]
    fn parses_neutrino_slug_runs_prs_and_mirrors() {
        assert_eq!(
            extract_ntr_slug("ssh://mini/Users/razeenwasif/.neutrino/repos/Neutron.git"),
            Some("neutron".to_owned())
        );
        assert_eq!(extract_ntr_slug("git@github.com:razeenwasif/Neutron.git"), None);

        let runs = "11    failure    CI   push   master  ff2d434  23 h ago\n9     success    CI   push   master  0ccee1b  3 d ago\n";
        assert_eq!(parse_ntr_runs(runs), Some(("11".to_owned(), CiState::Fail)));

        let prs = "#2   open   WIP feature   feat -> master   1h ago\n#1   merged Run CI   ci -> master   7d ago\n";
        assert_eq!(parse_ntr_open_prs(prs), 1);

        let mirror = "ok       neutron                  git@github.com:razeenwasif/Neutron.git       copied 23 h ago\n";
        assert_eq!(parse_ntr_mirror_ok(mirror, "neutron"), Some(true));
    }
}
