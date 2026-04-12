use std::path::Path;

pub(crate) use codex_protocol::permissions::FileSystemReadDenyMatcher as ReadDenyMatcher;
use codex_protocol::permissions::FileSystemSandboxPolicy;

use crate::function_tool::FunctionCallError;

const DENY_READ_POLICY_MESSAGE: &str =
    "access denied: reading this path is blocked by filesystem deny_read policy";

pub(crate) fn ensure_read_allowed(
    path: &Path,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    cwd: &Path,
) -> Result<(), FunctionCallError> {
    if file_system_sandbox_policy
        .read_deny_matcher_with_cwd(cwd)
        .is_some_and(|matcher| matcher.is_read_denied(path))
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "{DENY_READ_POLICY_MESSAGE}: `{}`",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn is_read_denied(
    path: &Path,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    cwd: &Path,
) -> bool {
    file_system_sandbox_policy
        .read_deny_matcher_with_cwd(cwd)
        .is_some_and(|matcher| matcher.is_read_denied(path))
}

#[cfg(test)]
mod tests {
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemPath;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::FileSystemSpecialPath;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::is_read_denied;
    use super::*;

    fn deny_policy(path: &std::path::Path) -> FileSystemSandboxPolicy {
        FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(path).expect("absolute deny path"),
            },
            access: FileSystemAccessMode::None,
        }])
    }

    fn root_deny_with_readable_carveout_policy(path: &std::path::Path) -> FileSystemSandboxPolicy {
        FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: AbsolutePathBuf::try_from(path).expect("absolute readable path"),
                },
                access: FileSystemAccessMode::Read,
            },
        ])
    }

    fn root_deny_with_readable_carveout_and_nested_deny_policy(
        readable_path: &std::path::Path,
        denied_path: &std::path::Path,
    ) -> FileSystemSandboxPolicy {
        FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: AbsolutePathBuf::try_from(readable_path).expect("absolute readable path"),
                },
                access: FileSystemAccessMode::Read,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: AbsolutePathBuf::try_from(denied_path).expect("absolute denied path"),
                },
                access: FileSystemAccessMode::None,
            },
        ])
    }

    #[test]
    fn exact_path_and_descendants_are_denied() {
        let temp = tempdir().expect("temp dir");
        let denied_dir = temp.path().join("denied");
        let nested = denied_dir.join("nested.txt");
        std::fs::create_dir_all(&denied_dir).expect("create denied dir");
        std::fs::write(&nested, "secret").expect("write secret");

        let policy = deny_policy(&denied_dir);
        assert_eq!(is_read_denied(&denied_dir, &policy, temp.path()), true);
        assert_eq!(is_read_denied(&nested, &policy, temp.path()), true);
        assert_eq!(
            is_read_denied(&temp.path().join("other.txt"), &policy, temp.path()),
            false
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_target_matches_denied_symlink_alias() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("temp dir");
        let real_dir = temp.path().join("real");
        let alias_dir = temp.path().join("alias");
        std::fs::create_dir_all(&real_dir).expect("create real dir");
        symlink(&real_dir, &alias_dir).expect("symlink alias");

        let secret = real_dir.join("secret.txt");
        std::fs::write(&secret, "secret").expect("write secret");
        let alias_secret = alias_dir.join("secret.txt");

        let policy = deny_policy(&real_dir);
        assert_eq!(is_read_denied(&alias_secret, &policy, temp.path()), true);
    }

    #[test]
    fn root_deny_blocks_paths_outside_readable_carveout() {
        let temp = tempdir().expect("temp dir");
        let readable_dir = temp.path().join("readable");
        let blocked_dir = temp.path().join("blocked");
        std::fs::create_dir_all(&readable_dir).expect("create readable dir");
        std::fs::create_dir_all(&blocked_dir).expect("create blocked dir");

        let policy = root_deny_with_readable_carveout_policy(&readable_dir);
        assert_eq!(
            is_read_denied(&blocked_dir.join("secret.txt"), &policy, temp.path()),
            true
        );
        assert_eq!(
            is_read_denied(&readable_dir.join("visible.txt"), &policy, temp.path()),
            false
        );
    }

    #[test]
    fn explicit_deny_inside_root_deny_carveout_still_wins() {
        let temp = tempdir().expect("temp dir");
        let readable_dir = temp.path().join("readable");
        let denied_dir = readable_dir.join("private");
        std::fs::create_dir_all(&denied_dir).expect("create denied dir");

        let policy =
            root_deny_with_readable_carveout_and_nested_deny_policy(&readable_dir, &denied_dir);
        assert_eq!(
            is_read_denied(&readable_dir.join("visible.txt"), &policy, temp.path()),
            false
        );
        assert_eq!(
            is_read_denied(&denied_dir.join("secret.txt"), &policy, temp.path()),
            true
        );
    }
}
