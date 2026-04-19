//! Utility to compute the current repository diff for the working directory.
//!
//! Supports Git repositories directly and Sapling repositories via the `sl`
//! CLI. The returned diff includes tracked changes plus untracked files when
//! possible. When the current directory is not inside a supported repository,
//! the function returns `Ok(None)`.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepoKind {
    Git,
    Sapling,
}

/// Return the current repository diff for the process working directory.
pub(crate) async fn get_repo_diff() -> io::Result<Option<String>> {
    let Some(repo_kind) = detect_repo_kind().await? else {
        return Ok(None);
    };

    let diff = match repo_kind {
        RepoKind::Git => get_git_diff().await?,
        RepoKind::Sapling => get_sapling_diff().await?,
    };
    Ok(Some(diff))
}

async fn detect_repo_kind() -> io::Result<Option<RepoKind>> {
    Ok(find_repo_kind_for_current_dir())
}

async fn get_git_diff() -> io::Result<String> {
    let (tracked_diff_res, untracked_output_res) = tokio::join!(
        run_capture_diff("git", &["diff", "--color"]),
        run_capture_stdout("git", &["ls-files", "--others", "--exclude-standard"]),
    );
    let tracked_diff = tracked_diff_res?;
    let untracked_output = untracked_output_res?;

    let mut combined_diff = String::new();
    append_diff_segment(&mut combined_diff, &tracked_diff);

    let null_device: &Path = if cfg!(windows) {
        Path::new("NUL")
    } else {
        Path::new("/dev/null")
    };

    let null_path = null_device.to_str().unwrap_or("/dev/null").to_string();
    let mut join_set: tokio::task::JoinSet<io::Result<String>> = tokio::task::JoinSet::new();
    for file in untracked_output
        .split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let null_path = null_path.clone();
        let file = file.to_string();
        join_set.spawn(async move {
            let args = ["diff", "--color", "--no-index", "--", &null_path, &file];
            run_capture_diff("git", &args).await
        });
    }
    while let Some(res) = join_set.join_next().await {
        match res {
            Ok(Ok(diff)) => append_diff_segment(&mut combined_diff, &diff),
            Ok(Err(err)) if err.kind() == io::ErrorKind::NotFound => {}
            Ok(Err(err)) => return Err(err),
            Err(_) => {}
        }
    }

    Ok(combined_diff)
}

async fn get_sapling_diff() -> io::Result<String> {
    let (tracked_diff_res, untracked_output_res, repo_root_res) = tokio::join!(
        run_capture_diff("sl", &["diff", "--git", "--color=always"]),
        run_capture_stdout(
            "sl",
            &[
                "status",
                "--unknown",
                "--no-status",
                "--root-relative",
                "--print0"
            ],
        ),
        sapling_root(),
    );
    let tracked_diff = tracked_diff_res?;
    let untracked_output = untracked_output_res?;
    let repo_root = repo_root_res?;

    let mut combined_diff = String::new();
    append_diff_segment(&mut combined_diff, &tracked_diff);

    for relative_path in parse_nul_delimited_paths(&untracked_output) {
        let absolute_path = repo_root.join(&relative_path);
        match build_untracked_file_diff(&relative_path, &absolute_path) {
            Ok(diff) => append_diff_segment(&mut combined_diff, &diff),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }

    Ok(combined_diff)
}

async fn run_capture_stdout(binary: &str, args: &[&str]) -> io::Result<String> {
    let output = Command::new(binary)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(io::Error::other(format!(
            "{binary} {:?} failed with status {}",
            args, output.status
        )))
    }
}

/// Like [`run_capture_stdout`] but treats exit status 1 as success and returns
/// stdout. Git/Sapling return 1 for diffs when differences are present.
async fn run_capture_diff(binary: &str, args: &[&str]) -> io::Result<String> {
    let output = Command::new(binary)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;

    if output.status.success() || output.status.code() == Some(1) {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(io::Error::other(format!(
            "{binary} {:?} failed with status {}",
            args, output.status
        )))
    }
}

fn find_repo_kind_for_current_dir() -> Option<RepoKind> {
    let cwd = std::env::current_dir().ok()?;
    find_repo_kind_for_path(&cwd)
}

fn find_repo_kind_for_path(path: &Path) -> Option<RepoKind> {
    let mut current = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };

    loop {
        if current.join(".git").exists() {
            return Some(RepoKind::Git);
        }
        if current.join(".sl").exists() {
            return Some(RepoKind::Sapling);
        }
        if !current.pop() {
            return None;
        }
    }
}

async fn sapling_root() -> io::Result<PathBuf> {
    let root = run_capture_stdout("sl", &["root"]).await?;
    let root = root.trim();
    if root.is_empty() {
        Err(io::Error::other(
            "sl root returned an empty repository path",
        ))
    } else {
        Ok(PathBuf::from(root))
    }
}

fn append_diff_segment(out: &mut String, segment: &str) {
    if segment.is_empty() {
        return;
    }

    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(segment);
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

fn parse_nul_delimited_paths(paths: &str) -> Vec<PathBuf> {
    paths
        .split('\0')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn build_untracked_file_diff(relative_path: &Path, absolute_path: &Path) -> io::Result<String> {
    let display_path = normalized_repo_path(relative_path);
    let mode = new_file_mode(absolute_path)?;
    let bytes = fs::read(absolute_path)?;

    if bytes.contains(&0) {
        return Ok(format!(
            "diff --git a/{display_path} b/{display_path}\nnew file mode {mode}\nBinary files /dev/null and b/{display_path} differ\n"
        ));
    }

    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(format!(
            "diff --git a/{display_path} b/{display_path}\nnew file mode {mode}\nBinary files /dev/null and b/{display_path} differ\n"
        ));
    };

    let diff_body = patch_body_for_added_text(&text);
    Ok(format!(
        "diff --git a/{display_path} b/{display_path}\nnew file mode {mode}\n--- /dev/null\n+++ b/{display_path}\n{diff_body}"
    ))
}

fn normalized_repo_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn patch_body_for_added_text(text: &str) -> String {
    let patch = diffy::create_patch("", text).to_string();
    let Some(first_newline) = patch.find('\n') else {
        return String::new();
    };
    let remainder = &patch[first_newline + 1..];
    let Some(second_newline) = remainder.find('\n') else {
        return String::new();
    };
    remainder[second_newline + 1..].to_string()
}

fn new_file_mode(path: &Path) -> io::Result<&'static str> {
    let metadata = fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 != 0 {
            Ok("100755")
        } else {
            Ok("100644")
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok("100644")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::env;

    use pretty_assertions::assert_eq;
    use serial_test::serial;
    use tempfile::tempdir;

    struct CurrentDirGuard {
        previous: PathBuf,
    }

    impl CurrentDirGuard {
        fn new(path: &Path) -> Self {
            let previous = env::current_dir().expect("read current dir");
            env::set_current_dir(path).expect("set current dir");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            env::set_current_dir(&self.previous).expect("restore current dir");
        }
    }

    struct PathGuard {
        previous: Option<String>,
    }

    impl PathGuard {
        fn prepend(path: &Path) -> Self {
            let previous = env::var("PATH").ok();
            let mut paths = vec![path.to_path_buf()];
            if let Some(existing) = &previous
                && !existing.is_empty()
            {
                paths.extend(env::split_paths(existing));
            }
            let joined = env::join_paths(paths).expect("join PATH entries");
            unsafe { env::set_var("PATH", joined) };
            Self { previous }
        }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(path) => unsafe { env::set_var("PATH", path) },
                None => unsafe { env::remove_var("PATH") },
            }
        }
    }

    #[test]
    fn parse_nul_delimited_paths_skips_empty_segments() {
        let paths = parse_nul_delimited_paths("foo\0bar/baz\0\0");
        assert_eq!(paths, vec![PathBuf::from("foo"), PathBuf::from("bar/baz")]);
    }

    #[test]
    fn find_repo_kind_prefers_git_marker() {
        let dir = tempdir().expect("create tempdir");
        fs::create_dir(dir.path().join(".git")).expect("create .git");
        fs::create_dir(dir.path().join(".sl")).expect("create .sl");

        assert_eq!(find_repo_kind_for_path(dir.path()), Some(RepoKind::Git));
    }

    #[test]
    fn find_repo_kind_uses_sapling_marker_when_git_missing() {
        let dir = tempdir().expect("create tempdir");
        fs::create_dir(dir.path().join(".sl")).expect("create .sl");

        assert_eq!(find_repo_kind_for_path(dir.path()), Some(RepoKind::Sapling));
    }

    #[test]
    fn build_untracked_file_diff_formats_text_files() {
        let dir = tempdir().expect("create tempdir");
        let file = dir.path().join("new.txt");
        fs::write(&file, "hello\nworld\n").expect("write file");

        let diff = build_untracked_file_diff(Path::new("new.txt"), &file)
            .expect("build untracked text diff");
        assert!(diff.contains("diff --git a/new.txt b/new.txt"));
        assert!(diff.contains("new file mode 100644"));
        assert!(diff.contains("--- /dev/null"));
        assert!(diff.contains("+++ b/new.txt"));
        assert!(diff.contains("+hello"));
        assert!(diff.contains("+world"));
    }

    #[test]
    fn build_untracked_file_diff_formats_binary_files() {
        let dir = tempdir().expect("create tempdir");
        let file = dir.path().join("image.bin");
        fs::write(&file, [0_u8, 159, 146, 150]).expect("write binary file");

        let diff = build_untracked_file_diff(Path::new("image.bin"), &file)
            .expect("build untracked binary diff");
        assert!(diff.contains("diff --git a/image.bin b/image.bin"));
        assert!(diff.contains("Binary files /dev/null and b/image.bin differ"));
    }

    #[tokio::test]
    #[serial]
    async fn get_repo_diff_returns_none_outside_supported_repo() {
        let dir = tempdir().expect("create tempdir");
        let _cwd = CurrentDirGuard::new(dir.path());

        assert_eq!(get_repo_diff().await.expect("compute repo diff"), None);
    }

    #[tokio::test]
    #[serial]
    async fn get_repo_diff_includes_git_untracked_files() {
        let dir = tempdir().expect("create tempdir");
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git init");
        Command::new("git")
            .args(["config", "user.name", "Codex Test"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git config user.name");
        Command::new("git")
            .args(["config", "user.email", "codex@example.com"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git config user.email");

        fs::write(dir.path().join("tracked.txt"), "one\n").expect("write tracked file");
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git commit");

        fs::write(dir.path().join("tracked.txt"), "one\ntwo\n").expect("update tracked file");
        fs::write(dir.path().join("untracked.txt"), "new\nfile\n").expect("write untracked file");

        let _cwd = CurrentDirGuard::new(dir.path());
        let diff = get_repo_diff()
            .await
            .expect("compute git diff")
            .expect("git repo should be supported");

        assert!(diff.contains("diff --git a/tracked.txt b/tracked.txt"));
        assert!(diff.contains("two"));
        assert!(diff.contains("diff --git a/untracked.txt b/untracked.txt"));
        assert!(diff.contains("+++ b/untracked.txt"));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn get_repo_diff_supports_sapling_repositories() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("create tempdir");
        fs::create_dir(dir.path().join(".sl")).expect("create .sl");
        fs::write(dir.path().join("unknown.txt"), "new\nfile\n").expect("write untracked file");

        let bin_dir = dir.path().join("bin");
        fs::create_dir(&bin_dir).expect("create bin dir");
        let script_path = bin_dir.join("sl");
        fs::write(
            &script_path,
            "#!/bin/sh\n\
            case \"$1\" in\n\
              root)\n\
                [ -d .sl ] || exit 1\n\
                pwd\n\
                ;;\n\
              diff)\n\
                [ -d .sl ] || exit 1\n\
                printf '%s' 'diff --git a/tracked.txt b/tracked.txt\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1,1 +1,2 @@\n one\n+two\n'\n\
                ;;\n\
              status)\n\
                [ -d .sl ] || exit 1\n\
                printf 'unknown.txt\\0'\n\
                ;;\n\
              *)\n\
                exit 1\n\
                ;;\n\
            esac\n",
        )
        .expect("write fake sl");
        let mut permissions = fs::metadata(&script_path)
            .expect("stat fake sl")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod fake sl");

        let _path = PathGuard::prepend(&bin_dir);
        let _cwd = CurrentDirGuard::new(dir.path());
        let diff = get_repo_diff()
            .await
            .expect("compute sapling diff")
            .expect("sapling repo should be supported");

        assert!(diff.contains("diff --git a/tracked.txt b/tracked.txt"));
        assert!(diff.contains("two"));
        assert!(diff.contains("diff --git a/unknown.txt b/unknown.txt"));
        assert!(diff.contains("+++ b/unknown.txt"));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn get_repo_diff_prefers_git_marker_when_sl_would_also_succeed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("create tempdir");
        let fake_bin_home = tempdir().expect("create fake bin tempdir");
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git init");
        Command::new("git")
            .args(["config", "user.name", "Codex Test"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git config user.name");
        Command::new("git")
            .args(["config", "user.email", "codex@example.com"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git config user.email");

        fs::write(dir.path().join("tracked.txt"), "one\n").expect("write tracked file");
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .await
            .expect("git commit");

        fs::write(dir.path().join("tracked.txt"), "one\ntwo\n").expect("update tracked file");

        let bin_dir = fake_bin_home.path().join("bin");
        fs::create_dir(&bin_dir).expect("create bin dir");
        let script_path = bin_dir.join("sl");
        fs::write(
            &script_path,
            "#!/bin/sh\n\
            case \"$1\" in\n\
              root)\n\
                pwd\n\
                ;;\n\
              diff)\n\
                printf '%s' 'diff --git a/not-git.txt b/not-git.txt\n--- a/not-git.txt\n+++ b/not-git.txt\n@@ -1,1 +1,1 @@\n-fake\n+sapling\n'\n\
                ;;\n\
              status)\n\
                printf ''\n\
                ;;\n\
              *)\n\
                exit 1\n\
                ;;\n\
            esac\n",
        )
        .expect("write fake sl");
        let mut permissions = fs::metadata(&script_path)
            .expect("stat fake sl")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod fake sl");

        let _path = PathGuard::prepend(&bin_dir);
        let _cwd = CurrentDirGuard::new(dir.path());
        let diff = get_repo_diff()
            .await
            .expect("compute repo diff")
            .expect("git repo should be supported");

        assert!(diff.contains("diff --git a/tracked.txt b/tracked.txt"));
        assert!(!diff.contains("diff --git a/not-git.txt b/not-git.txt"));
    }
}
