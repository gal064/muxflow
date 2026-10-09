use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::path_policy::visit_directory_entries;
use super::*;

const MAX_RESULTS: usize = 75;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 - 1024;
const MAX_VISITED: usize = 50_000;
const MAX_DEPTH: usize = 64;
const SEARCH_BUDGET: Duration = Duration::from_millis(500);
const MAX_IGNORE_BYTES: u64 = 64 * 1024;

impl FileService {
    pub(crate) fn search_files(
        &self,
        root: &str,
        root_token: &str,
        cwd: &str,
        query: &str,
        cancellation: &AtomicBool,
    ) -> anyhow::Result<v1::FileSearchResults> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(v1::FileSearchResults {
                matches: vec![],
                complete: true,
            });
        }
        if query.len() > 1024 {
            bail!("file search query is too long");
        }
        if cancellation.load(Ordering::Acquire) {
            return Err(cancelled("file search"));
        }
        let started = Instant::now();
        let root = RootCapability::validate(root, root_token)?;
        let (cwd, directory) = resolve_watch_directory(&root, cwd)?;
        let directory_identity = directory.metadata()?;
        let mut ignores = Vec::new();
        let excludes = super::super::git::file_search_excludes(
            root.logical_root()
                .to_str()
                .context("search root must be UTF-8")?,
            cancellation,
        );
        if cancellation.load(Ordering::Acquire) {
            return Err(cancelled("file search"));
        }
        let (sources, case_insensitive) = excludes?;
        for content in sources {
            if cancellation.load(Ordering::Acquire) {
                return Err(cancelled("file search"));
            }
            ignores.push(ignore_content(
                root.logical_root(),
                &content,
                case_insensitive,
            )?);
        }
        // Serialize enumeration only: cancelled Git lookups may still be
        // reaping their process group and must not hold up the next query.
        let _search = self.search_lock.lock().unwrap();
        let results = search(
            &root,
            &cwd,
            directory,
            query,
            cancellation,
            MAX_VISITED,
            SEARCH_BUDGET,
            ignores,
            started,
            case_insensitive,
        )?;
        validate_search_directory(&root, &cwd, &directory_identity)?;
        Ok(results)
    }
}

fn validate_search_directory(
    root: &RootCapability,
    cwd: &Path,
    captured: &Metadata,
) -> anyhow::Result<()> {
    let fresh_root = RootCapability::validate(
        root.logical_root()
            .to_str()
            .context("search root must be UTF-8")?,
        root.token(),
    )?;
    let (_, directory) = resolve_watch_directory(
        &fresh_root,
        cwd.to_str().context("search cwd must be UTF-8")?,
    )?;
    let fresh = directory.metadata()?;
    if fresh.dev() != captured.dev() || fresh.ino() != captured.ino() {
        bail!("file search working directory changed");
    }
    Ok(())
}

struct Search<'a> {
    query: String,
    cwd: &'a Path,
    cancellation: &'a AtomicBool,
    started: Instant,
    budget: Duration,
    max_visited: usize,
    visited: usize,
    stopped: bool,
    limited: bool,
    matches: Vec<v1::FileSearchMatch>,
    case_insensitive: bool,
}

impl Search<'_> {
    fn keep_going(&mut self) -> anyhow::Result<bool> {
        if self.cancellation.load(Ordering::Acquire) {
            return Err(cancelled("file search"));
        }
        if self.visited >= self.max_visited || self.started.elapsed() >= self.budget {
            self.stopped = true;
        }
        Ok(!self.stopped)
    }

    fn walk(
        &mut self,
        directory: File,
        path: &Path,
        ignores: &mut Vec<Gitignore>,
        depth: usize,
    ) -> anyhow::Result<()> {
        if !self.keep_going()? {
            return Ok(());
        }
        if depth >= MAX_DEPTH {
            self.limited = true;
            return Ok(());
        }
        let count = ignores.len();
        if let Some(ignore) = read_ignore(&directory, path, self.case_insensitive)? {
            ignores.push(ignore);
        }
        visit_directory_entries(&directory, |name| {
            if !self.keep_going()? {
                return Ok(false);
            }
            self.visited += 1;
            if is_never_enumerated(&name) {
                return Ok(true);
            }
            let entry = AnchoredPath::in_directory(&directory, name.clone())?;
            let metadata = match entry.metadata_no_follow() {
                Ok(metadata) => metadata,
                Err(error) if listing::entry_vanished(&error) => return Ok(true),
                Err(_) => {
                    self.limited = true;
                    return Ok(true);
                }
            };
            let logical = path.join(&name);
            // Directory symlinks are never traversed; file symlinks are opened
            // only through the existing explicit-path resolver, not enumerated.
            if !metadata.is_file() && !metadata.is_dir() {
                return Ok(true);
            }
            for ignore in ignores.iter().rev() {
                let matched = ignore.matched(&logical, metadata.is_dir());
                if matched.is_ignore() {
                    return Ok(true);
                }
                if matched.is_whitelist() {
                    break;
                }
            }
            if metadata.is_dir() {
                match entry.open_directory() {
                    Ok(child) => self.walk(child, &logical, ignores, depth + 1)?,
                    Err(error) if listing::entry_vanished(&error) => {}
                    Err(_) => self.limited = true,
                }
            } else if let Some(relative) =
                logical.strip_prefix(self.cwd).ok().and_then(Path::to_str)
            {
                let Some(name) = name.to_str() else {
                    return Ok(true);
                };
                if let Some(score) = search_score(name, relative, &self.query) {
                    let Some(path) = logical.to_str() else {
                        return Ok(true);
                    };
                    self.matches.push(v1::FileSearchMatch {
                        path: path.to_owned(),
                        relative_path: relative.to_owned(),
                        score,
                    });
                    self.matches.sort_by(|a, b| {
                        b.score
                            .cmp(&a.score)
                            .then_with(|| a.relative_path.cmp(&b.relative_path))
                    });
                    if self.matches.len() > MAX_RESULTS {
                        self.matches.pop();
                        self.limited = true;
                    }
                }
            }
            Ok(!self.stopped)
        })?;
        ignores.truncate(count);
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)] // Explicit bounds let tests exercise the same traversal.
fn search(
    root: &RootCapability,
    cwd: &Path,
    directory: File,
    query: &str,
    cancellation: &AtomicBool,
    max_visited: usize,
    budget: Duration,
    mut ignores: Vec<Gitignore>,
    started: Instant,
    case_insensitive: bool,
) -> anyhow::Result<v1::FileSearchResults> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(v1::FileSearchResults {
            matches: vec![],
            complete: true,
        });
    }
    if query.len() > 1024 {
        bail!("file search query is too long");
    }
    let mut search = Search {
        query: query.to_lowercase(),
        cwd,
        cancellation,
        started,
        budget,
        max_visited,
        visited: 0,
        stopped: false,
        limited: false,
        matches: vec![],
        case_insensitive,
    };
    // Inherit repository rules above a nested cwd without enumerating siblings.
    let mut ancestor = root.logical_root().to_owned();
    for component in cwd.strip_prefix(root.logical_root())?.components() {
        if !search.keep_going()? {
            break;
        }
        let dir = if ancestor == root.logical_root() {
            root.open_root_directory()?
        } else {
            root.anchor(&ancestor)?.open_directory()?
        };
        if let Some(ignore) = read_ignore(&dir, &ancestor, case_insensitive)? {
            ignores.push(ignore);
        }
        ancestor.push(component);
        for ignore in ignores.iter().rev() {
            let matched = ignore.matched(&ancestor, true);
            if matched.is_ignore() {
                return Ok(v1::FileSearchResults {
                    matches: vec![],
                    complete: true,
                });
            }
            if matched.is_whitelist() {
                break;
            }
        }
    }
    search.walk(directory, cwd, &mut ignores, 0)?;
    let mut bytes = 0;
    search.matches.retain(|item| {
        let size = serde_json::to_vec(&serde_json::json!({
            "path": item.path, "relativePath": item.relative_path, "score": item.score,
        }))
        .expect("string-only search match serializes")
        .len()
            + 1;
        if bytes + size > MAX_RESPONSE_BYTES {
            search.limited = true;
            false
        } else {
            bytes += size;
            true
        }
    });
    Ok(v1::FileSearchResults {
        matches: search.matches,
        complete: !search.stopped && !search.limited,
    })
}

fn read_ignore(
    directory: &File,
    path: &Path,
    case_insensitive: bool,
) -> anyhow::Result<Option<Gitignore>> {
    let entry = AnchoredPath::in_directory(directory, OsString::from(".gitignore"))?;
    let metadata = match entry.metadata_no_follow() {
        Ok(metadata) => metadata,
        Err(error) if listing::entry_vanished(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    let mut content = String::new();
    entry
        .open_file()?
        .take(MAX_IGNORE_BYTES + 1)
        .read_to_string(&mut content)?;
    if content.len() as u64 > MAX_IGNORE_BYTES {
        bail!(".gitignore exceeds the file search limit");
    }
    Ok(Some(ignore_content(path, &content, case_insensitive)?))
}

fn ignore_content(path: &Path, content: &str, case_insensitive: bool) -> anyhow::Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(path);
    builder.case_insensitive(case_insensitive)?;
    for line in content.lines() {
        builder.add_line(Some(path.join(".gitignore")), line)?;
    }
    Ok(builder.build()?)
}

/// Same subsequence score as the desktop matcher. Names outrank path-only hits.
fn search_score(name: &str, path: &str, query: &str) -> Option<i32> {
    fuzzy_score(name, query)
        .map(|score| score + 1000)
        .or_else(|| fuzzy_score(path, query))
}

fn fuzzy_score(text: &str, query: &str) -> Option<i32> {
    let chars: Vec<char> = text.to_lowercase().chars().collect();
    let mut cursor = 0;
    let mut previous = -2_i32;
    let mut score = 0;
    for character in query.chars().filter(|c| *c != ' ') {
        let found = chars[cursor..].iter().position(|c| *c == character)? + cursor;
        let position = found as i32;
        if position == previous + 1 {
            score += 8;
        } else {
            score -= (position - previous - 1).min(10);
        }
        if found == 0 {
            score += 12;
        } else if matches!(chars[found - 1], ' ' | '/' | '-' | '_' | '.' | ':') {
            score += 6;
        }
        previous = position;
        cursor = found + 1;
    }
    Some(score - (chars.len() / 12) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, cwd: &Path, query: &str, limit: usize) -> v1::FileSearchResults {
        let root = RootCapability::capture(root.to_str().unwrap()).unwrap();
        let (_, directory) = resolve_watch_directory(&root, cwd.to_str().unwrap()).unwrap();
        search(
            &root,
            cwd,
            directory,
            query,
            &AtomicBool::new(false),
            limit,
            SEARCH_BUDGET,
            vec![],
            Instant::now(),
            false,
        )
        .unwrap()
    }

    #[test]
    fn cwd_ignore_negation_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let cwd = root.join("src");
        fs::create_dir_all(cwd.join("node_modules")).unwrap();
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        fs::write(cwd.join(".gitignore"), "!keep.log\n").unwrap();
        for path in [
            "top.txt",
            "src/file.txt",
            "src/skip.log",
            "src/keep.log",
            "src/node_modules/dependency.txt",
        ] {
            fs::write(root.join(path), "content that must not be searched").unwrap();
        }
        std::os::unix::fs::symlink(&root, cwd.join("outside")).unwrap();
        let results = run(&root, &cwd, "txt", MAX_VISITED);
        assert_eq!(
            results
                .matches
                .iter()
                .map(|item| item.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["file.txt"]
        );
        assert!(results.complete);
        assert_eq!(
            run(&root, &cwd, "log", MAX_VISITED).matches[0].relative_path,
            "keep.log"
        );
        assert!(run(&root, &cwd, "content", MAX_VISITED).matches.is_empty());
    }

    #[test]
    fn limits_cancellation_and_name_ranking() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for index in 0..100 {
            fs::write(root.join(format!("file-{index}.txt")), "").unwrap();
        }
        let results = run(&root, &root, "file", MAX_VISITED);
        assert_eq!(results.matches.len(), MAX_RESULTS);
        assert!(!results.complete);
        assert!(!run(&root, &root, "missing", 5).complete);
        let service = FileService::new();
        let token = root_token(root.to_str().unwrap()).unwrap();
        assert!(
            service
                .search_files(
                    root.to_str().unwrap(),
                    &token,
                    root.to_str().unwrap(),
                    "file",
                    &AtomicBool::new(true)
                )
                .is_err()
        );
        assert!(
            search_score("file.ts", "deep/file.ts", "file").unwrap()
                > search_score("other.ts", "file/other.ts", "file").unwrap()
        );
        assert_eq!(fuzzy_score("WorkspaceSwitcher.tsx", "work"), Some(34));
    }

    #[test]
    fn root_scope_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let service = FileService::new();
        let token = root_token(root.to_str().unwrap()).unwrap();
        assert!(
            service
                .search_files(
                    root.to_str().unwrap(),
                    &token,
                    outside.path().to_str().unwrap(),
                    "x",
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        assert!(
            service
                .search_files(
                    root.to_str().unwrap(),
                    "stale",
                    root.to_str().unwrap(),
                    "x",
                    &AtomicBool::new(false)
                )
                .is_err()
        );
    }

    #[test]
    fn starting_inside_an_ignored_directory_does_not_enumerate_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let cwd = root.join("dist");
        fs::create_dir(&cwd).unwrap();
        fs::write(root.join(".gitignore"), "dist/\n").unwrap();
        fs::write(cwd.join("file.txt"), "").unwrap();
        let results = run(&root, &cwd, "file", MAX_VISITED);
        assert!(results.complete);
        assert!(results.matches.is_empty());
    }

    #[test]
    fn repository_excludes_apply_to_linked_worktrees() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("main");
        let linked = temp.path().join("linked");
        fs::create_dir(&root).unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(args)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        fs::write(root.join("tracked.txt"), "").unwrap();
        git(&["add", "tracked.txt"]);
        git(&[
            "-c",
            "user.name=QA",
            "-c",
            "user.email=qa@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ]);
        git(&["worktree", "add", "--detach", linked.to_str().unwrap()]);
        fs::write(root.join(".git/info/exclude"), "skip.txt\n").unwrap();
        fs::write(linked.join("skip.txt"), "").unwrap();
        let linked = fs::canonicalize(linked).unwrap();
        let root = linked.to_str().unwrap();
        let service = FileService::new();
        let results = service
            .search_files(
                root,
                &root_token(root).unwrap(),
                root,
                "txt",
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(
            results
                .matches
                .iter()
                .map(|item| item.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["tracked.txt"]
        );
    }

    #[test]
    fn effective_config_includes_and_oversized_ignore_sources() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        let source = root.join("custom-ignore");
        let link = root.join("ignore-link");
        fs::write(&source, "ignored/\n").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let config = root.join("included-config");
        fs::write(
            &config,
            format!("[core]\nexcludesFile = {}\n", link.display()),
        )
        .unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["config", "include.path", config.to_str().unwrap()])
                .status()
                .unwrap()
                .success()
        );
        fs::create_dir(root.join("ignored")).unwrap();
        fs::write(root.join("ignored/file-secret.txt"), "").unwrap();
        fs::write(root.join("file-visible.txt"), "").unwrap();
        let path = root.to_str().unwrap();
        let token = root_token(path).unwrap();
        let service = FileService::new();
        let results = service
            .search_files(path, &token, path, "file", &AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            results
                .matches
                .iter()
                .map(|item| item.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["file-visible.txt"]
        );
        fs::write(&source, "#".repeat(65_537)).unwrap();
        assert!(
            service
                .search_files(path, &token, path, "file", &AtomicBool::new(false))
                .unwrap_err()
                .to_string()
                .contains("exceed")
        );
    }

    #[test]
    fn cancelled_request_does_not_wait_for_another_walk() {
        let service = Arc::new(FileService::new());
        let guard = service.search_lock.lock().unwrap();
        let worker_service = Arc::clone(&service);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = worker_service.search_files(
                "/missing-search-root",
                "invalid",
                "/missing-search-root",
                "file",
                &AtomicBool::new(true),
            );
            tx.send(result).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_secs(1));
        drop(guard);
        worker.join().unwrap();
        assert_eq!(
            FileFailure::of(&result.unwrap().unwrap_err()),
            FileFailure::Cancelled
        );
    }

    #[test]
    fn empty_query_never_opens_a_root_or_reads_git_configuration() {
        let result = FileService::new()
            .search_files(
                "/missing-search-root",
                "invalid",
                "/missing-search-root",
                " \t ",
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(result.complete && result.matches.is_empty());
    }

    #[test]
    fn effective_case_insensitive_rules_and_disabled_global_excludes() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "core.ignorecase", "true"],
            vec!["config", "core.excludesFile", ""],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::create_dir(root.join("Build")).unwrap();
        fs::write(root.join("Build/file-secret.txt"), "").unwrap();
        fs::write(root.join("file-visible.txt"), "").unwrap();
        fs::write(root.join(".gitignore"), "build/\n").unwrap();
        let path = root.to_str().unwrap();
        let results = FileService::new()
            .search_files(
                path,
                &root_token(path).unwrap(),
                path,
                "file",
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(
            results
                .matches
                .iter()
                .map(|item| item.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["file-visible.txt"]
        );
        fs::remove_file(root.join(".gitignore")).unwrap();
        fs::write(root.join(".git/info/exclude"), "build/\n").unwrap();
        let results = FileService::new()
            .search_files(
                path,
                &root_token(path).unwrap(),
                path,
                "file",
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(results.matches.len(), 1);
        assert_eq!(results.matches[0].relative_path, "file-visible.txt");
    }

    #[test]
    fn rejects_replaced_root_and_nested_cwd_after_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        fs::create_dir_all(path.join("cwd")).unwrap();
        let root = RootCapability::capture(path.to_str().unwrap()).unwrap();
        let cwd = root.logical_root().join("cwd");
        let (_, directory) = resolve_watch_directory(&root, cwd.to_str().unwrap()).unwrap();
        let identity = directory.metadata().unwrap();
        validate_search_directory(&root, &cwd, &identity).unwrap();
        fs::rename(&cwd, path.join("old-cwd")).unwrap();
        fs::create_dir(&cwd).unwrap();
        assert!(validate_search_directory(&root, &cwd, &identity).is_err());
        let identity = fs::metadata(&cwd).unwrap();
        fs::rename(&path, temp.path().join("old-root")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        assert!(validate_search_directory(&root, &cwd, &identity).is_err());
    }

    #[test]
    fn long_paths_cannot_exceed_the_control_response_budget() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let mut nested = root.clone();
        for _ in 0..10 {
            nested.push("a".repeat(200));
        }
        fs::create_dir_all(&nested).unwrap();
        for index in 0..80 {
            fs::write(nested.join(format!("file-{index}.txt")), "").unwrap();
        }
        let results = run(&root, &root, "file", MAX_VISITED);
        assert!(!results.complete);
        assert!(!results.matches.is_empty());
        let json = serde_json::json!({ "matches": results.matches.iter().map(|item| serde_json::json!({
            "path": item.path, "relativePath": item.relative_path, "score": item.score,
        })).collect::<Vec<_>>(), "complete": results.complete });
        assert!(serde_json::to_vec(&json).unwrap().len() < 32 * 1024);
        let capability = RootCapability::capture(root.to_str().unwrap()).unwrap();
        let results = search(
            &capability,
            &root,
            capability.open_root_directory().unwrap(),
            "file",
            &AtomicBool::new(false),
            MAX_VISITED,
            Duration::ZERO,
            vec![],
            Instant::now(),
            false,
        )
        .unwrap();
        assert!(!results.complete);
        assert!(results.matches.is_empty());
    }
}
