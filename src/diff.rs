//! git diff collection + repo browsing for the Phase 4 review surfaces.
//!
//! Everything here shells out to `git` with `-C <dir>` and plain argument
//! arrays (never a shell), so paths with spaces or shell metacharacters stay
//! inert. The gateway runs these on the user's own machine behind an SSH
//! login, which is already full remote access — the only boundary this module
//! adds is *file* access: `/browse/file` reads are confined to the git work
//! tree so a page rendered in the app's WebView can't turn the forward into
//! an arbitrary file reader.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::Context;

/// One changed file in the working tree / index / against a commit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Change {
    /// Path relative to the repo root (the `b/` side of the diff).
    pub name: String,
    /// Previous path, present for renames.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_name: Option<String>,
    pub status: ChangeStatus,
    /// Unified diff text for this file (hunks only for untracked files,
    /// which git can't diff — they carry `contents` instead).
    pub patch: String,
    /// Full contents for untracked files; `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

/// Cap on a single untracked file's contents. Anything bigger is listed but
/// not shipped to the phone — the WebView would choke long before the JSON.
const MAX_UNTRACKED_BYTES: usize = 1 << 20;

/// Collect the changes in `dir` for the requested scopes. `commit` (when
/// given) diffs the working tree against that ref and wins over the staged /
/// unstaged flags. Returns `Ok(None)` when `dir` is not a git repository.
pub fn collect_changes(
    dir: &Path,
    staged: bool,
    unstaged: bool,
    untracked: bool,
    commit: Option<&str>,
) -> anyhow::Result<Option<Vec<Change>>> {
    let dir = canonicalize_dir(dir)?;
    if !is_git_repo(&dir)? {
        return Ok(None);
    }

    let mut changes = Vec::new();

    if let Some(commit) = commit {
        let patch = git(&dir, ["diff", "--no-color", commit])?;
        changes.extend(parse_diff(&patch));
    } else {
        if staged {
            let patch = git(&dir, ["diff", "--cached", "--no-color", "--"])?;
            changes.extend(parse_diff(&patch));
        }
        if unstaged {
            let patch = git(&dir, ["diff", "--no-color", "--"])?;
            changes.extend(parse_diff(&patch));
        }
    }

    if untracked {
        for name in untracked_files(&dir)? {
            let path = resolve_in(&dir, &name)?;
            let contents = match std::fs::read(&path) {
                Ok(bytes) if bytes.len() <= MAX_UNTRACKED_BYTES => {
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                _ => String::new(),
            };
            changes.push(Change {
                name,
                prev_name: None,
                status: ChangeStatus::Untracked,
                patch: String::new(),
                contents: Some(contents),
            });
        }
    }

    Ok(Some(changes))
}

/// File list at a commit (or the working tree when `commit` is `None`).
pub fn list_files(dir: &Path, commit: Option<&str>) -> anyhow::Result<Option<Vec<String>>> {
    let dir = canonicalize_dir(dir)?;
    if !is_git_repo(&dir)? {
        return Ok(None);
    }
    let output = match commit {
        Some(commit) => git(&dir, ["ls-tree", "-r", "--name-only", commit])?,
        None => {
            // Untracked files don't exist in ls-tree; walk the working tree
            // and merge.
            let tracked = git(&dir, ["ls-tree", "-r", "--name-only", "HEAD"])?;
            let mut files: Vec<String> = tracked.lines().map(str::to_owned).collect();
            for untracked in untracked_files(&dir)? {
                if !files.iter().any(|f| f == &untracked) {
                    files.push(untracked);
                }
            }
            files.sort();
            return Ok(Some(files));
        }
    };
    Ok(Some(output.lines().map(str::to_owned).collect()))
}

/// Read one file's contents at a commit, or from the working tree when
/// `commit` is `None` (untracked files). Confined to the repo root. Returns
/// `Ok(None)` when the file doesn't exist there.
pub fn read_file(
    dir: &Path,
    commit: Option<&str>,
    rel: &str,
) -> anyhow::Result<Option<String>> {
    let dir = canonicalize_dir(dir)?;
    if !is_git_repo(&dir)? {
        return Ok(None);
    }
    match commit {
        Some(commit) => {
            let output = git_optional(&dir, ["show", &format!("{commit}:{rel}")])?;
            match output {
                Some(text) => Ok(Some(text)),
                None => Ok(None),
            }
        }
        None => {
            let path = resolve_in(&dir, rel)?;
            if !path.is_file() {
                return Ok(None);
            }
            let bytes = std::fs::read(&path)?;
            if bytes.len() > MAX_UNTRACKED_BYTES {
                return Ok(None);
            }
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        }
    }
}

// ---------------------------------------------------------------------------
// git plumbing
// ---------------------------------------------------------------------------

fn git<I, S>(dir: &Path, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    git_optional(dir, args).map(|o| o.unwrap_or_default())
}

/// Run git in `dir`; `Ok(None)` means the command failed (bad ref, missing
/// file, permission…) rather than crashing the request.
fn git_optional<I, S>(dir: &Path, args: I) -> anyhow::Result<Option<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

fn is_git_repo(dir: &Path) -> anyhow::Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to run git")?;
    Ok(output.status.success())
}

fn untracked_files(dir: &Path) -> anyhow::Result<Vec<String>> {
    let output = git(dir, ["ls-files", "--others", "--exclude-standard"])?;
    Ok(output.lines().map(str::to_owned).collect())
}

// ---------------------------------------------------------------------------
// diff parsing
// ---------------------------------------------------------------------------

/// Split a combined `git diff` output into per-file chunks at `diff --git`
/// boundaries. Files with unusual names (git quotes them) parse with a
/// best-effort name and still render — the patch text is what matters.
fn parse_diff(patch: &str) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut current: Option<String> = None;
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            if let Some(chunk) = current.take() {
                if let Some(change) = parse_chunk(&chunk) {
                    changes.push(change);
                }
            }
            current = Some(line.to_owned());
        } else if let Some(chunk) = current.as_mut() {
            chunk.push('\n');
            chunk.push_str(line);
        }
    }
    if let Some(chunk) = current.take() {
        if let Some(change) = parse_chunk(&chunk) {
            changes.push(change);
        }
    }
    changes
}

fn parse_chunk(chunk: &str) -> Option<Change> {
    let header = chunk.lines().next()?;
    let (old, new) = parse_diffgit_header(header)?;
    let lower = chunk.to_ascii_lowercase();
    let status = if lower.contains("new file mode") {
        ChangeStatus::Added
    } else if lower.contains("deleted file mode") {
        ChangeStatus::Deleted
    } else if lower.contains("rename from") {
        ChangeStatus::Renamed
    } else {
        ChangeStatus::Modified
    };
    let is_rename = old != new;
    Some(Change {
        name: new.clone(),
        prev_name: is_rename.then_some(old),
        status,
        patch: chunk.to_owned(),
        contents: None,
    })
}

/// `diff --git a/<old> b/<new>` → (old, new). The ` b/` split is the one
/// ambiguity (an `a/` path containing ` b/`); git quotes such names, so the
/// unquoted fast path stays correct for everything else.
fn parse_diffgit_header(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("diff --git ")?;
    let (old, new) = rest.split_once(" b/")?;
    let old = old.strip_prefix("a/").unwrap_or(old);
    Some((old.to_owned(), new.to_owned()))
}

// ---------------------------------------------------------------------------
// path confinement
// ---------------------------------------------------------------------------

fn canonicalize_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    let dir = dir.canonicalize().with_context(|| {
        format!("path does not exist: {}", dir.display())
    })?;
    if !dir.is_dir() {
        anyhow::bail!("not a directory: {}", dir.display());
    }
    Ok(dir)
}

/// Resolve `rel` inside `root`, refusing to escape it. `rel` may contain
/// nested separators but must stay within `root` after normalization.
fn resolve_in(root: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let joined = root.join(rel);
    let canonical = joined
        .canonicalize()
        .with_context(|| format!("no such file: {rel}"))?;
    if !canonical.starts_with(root) {
        anyhow::bail!("path escapes repo root: {rel}");
    }
    if !canonical
        .components()
        .any(|c| matches!(c, Component::Normal(_)))
    {
        anyhow::bail!("empty path");
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo {
        dir: tempfile::TempDir,
    }

    impl Repo {
        fn new() -> Repo {
            let dir = tempfile::tempdir().unwrap();
            git(&dir.path(), ["init", "-q"]).unwrap();
            git(
                &dir.path(),
                ["config", "user.email", "test@example.com"],
            )
            .unwrap();
            git(&dir.path(), ["config", "user.name", "test"]).unwrap();
            Repo { dir }
        }

        fn write(&self, name: &str, contents: &str) {
            let path = self.dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        fn commit_all(&self, message: &str) {
            git(&self.dir.path(), ["add", "-A"]).unwrap();
            git(
                &self.dir.path(),
                ["commit", "-q", "-m", message],
            )
            .unwrap();
        }
    }

    #[test]
    fn collects_staged_unstaged_and_untracked() {
        let repo = Repo::new();
        repo.write("a.txt", "one\n");
        repo.write("b.txt", "keep\n");
        repo.commit_all("initial");

        repo.write("a.txt", "one\nchanged\n");
        git(&repo.dir.path(), ["add", "a.txt"]).unwrap();
        repo.write("b.txt", "keep\nalso\n");
        repo.write("new.txt", "brand new\n");

        let changes =
            collect_changes(repo.dir.path(), true, true, true, None)
                .unwrap()
                .unwrap();

        for c in &changes {
            eprintln!("Change: {} ({:?}), patch_len={}", c.name, c.status, c.patch.len());
            eprintln!("Patch:\n{}", c.patch);
        }

        let a = changes.iter().find(|c| c.name == "a.txt").unwrap();
        assert_eq!(a.status, ChangeStatus::Modified);
        // Staged diff: context line " one" + addition "+changed"
        assert!(a.patch.contains(" one") && a.patch.contains("+changed"));

        let b = changes.iter().find(|c| c.name == "b.txt").unwrap();
        assert!(b.patch.contains("+also"));

        let new = changes.iter().find(|c| c.name == "new.txt").unwrap();
        assert_eq!(new.status, ChangeStatus::Untracked);
        assert_eq!(new.contents.as_deref(), Some("brand new\n"));
    }

    #[test]
    fn detects_added_deleted_renamed() {
        let repo = Repo::new();
        repo.write("keep.txt", "keep\n");
        repo.commit_all("initial");

        repo.write("added.txt", "hi\n");
        // git mv needs the source file to exist, so rename before rm
        git(&repo.dir.path(), ["mv", "keep.txt", "moved.txt"]).unwrap();
        std::fs::remove_file(repo.dir.path().join("keep.txt")).ok();
        git(&repo.dir.path(), ["add", "-A"]).unwrap();

        let changes =
            collect_changes(repo.dir.path(), true, false, false, None)
                .unwrap()
                .unwrap();

        let added = changes.iter().find(|c| c.name == "added.txt").unwrap();
        assert_eq!(added.status, ChangeStatus::Added);

        let moved = changes.iter().find(|c| c.name == "moved.txt").unwrap();
        assert_eq!(moved.status, ChangeStatus::Renamed);
        assert_eq!(moved.prev_name.as_deref(), Some("keep.txt"));
    }

    #[test]
    fn commit_diff_overrides_flags() {
        let repo = Repo::new();
        repo.write("a.txt", "one\n");
        repo.commit_all("first");
        repo.write("a.txt", "two\n");
        repo.commit_all("second");

        let changes =
            collect_changes(repo.dir.path(), false, false, false, Some("HEAD~1"))
                .unwrap()
                .unwrap();
        assert_eq!(changes.len(), 1);
        assert!(changes[0].patch.contains("-one") && changes[0].patch.contains("+two"));
    }

    #[test]
    fn non_git_dir_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.txt"), "x").unwrap();
        assert!(
            collect_changes(dir.path(), true, true, true, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn browse_lists_and_reads_files() {
        let repo = Repo::new();
        repo.write("src/a.ts", "export const a = 1;\n");
        repo.commit_all("initial");

        let files = list_files(repo.dir.path(), None).unwrap().unwrap();
        assert!(files.iter().any(|f| f == "src/a.ts"));

        let contents =
            read_file(repo.dir.path(), Some("HEAD"), "src/a.ts")
                .unwrap()
                .unwrap();
        assert_eq!(contents, "export const a = 1;\n");
    }

    #[test]
    fn read_file_refuses_escape() {
        let repo = Repo::new();
        repo.write("a.txt", "inside\n");
        repo.commit_all("initial");

        // The path string is validated before git ever sees it: resolve_in
        // fails because it escapes the repo root.
        let result = read_file(repo.dir.path(), None, "../outside.txt");
        assert!(result.is_err() || result.unwrap().is_none());
    }
}
