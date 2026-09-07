//! Embed the collab skill bundle and install it into a target skills
//! directory. The canonical source of truth lives under
//! `skills/collab/` in the repository; `include_str!` pulls every file
//! into the binary at compile time so `collab install-skills` is a
//! single-step deployment alongside `cargo install`.

#[allow(dead_code)]
pub const SKILL_ROOT: &str = "skills/collab";

pub const SKILL_FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../skills/collab/SKILL.md"),
    ),
    (
        "references/migration-daemon.md",
        include_str!("../skills/collab/references/migration-daemon.md"),
    ),
    (
        "references/notifications.md",
        include_str!("../skills/collab/references/notifications.md"),
    ),
    (
        "references/resource-waits.md",
        include_str!("../skills/collab/references/resource-waits.md"),
    ),
    (
        "references/task-worktree-lifecycle.md",
        include_str!("../skills/collab/references/task-worktree-lifecycle.md"),
    ),
    (
        "references/verification.md",
        include_str!("../skills/collab/references/verification.md"),
    ),
];

/// Result of installing a single skill file.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Written,
    Skipped,
}

/// Install the embedded skill bundle into `target`. With `force=false`
/// an existing file is left in place and reported as `Skipped`; with
/// `force=true` it is overwritten. Returns the per-file outcomes plus
/// the total embedded byte count and file count.
pub fn install(
    target: &std::path::Path,
    force: bool,
) -> Result<(Vec<(&'static str, InstallOutcome)>, usize, usize), String> {
    if target.as_os_str().is_empty() {
        return Err("install-skills target must be a non-empty path".into());
    }
    let bytes_total = SKILL_FILES.iter().map(|(_, body)| body.len()).sum::<usize>();
    let mut outcomes = Vec::with_capacity(SKILL_FILES.len());
    for (relative, body) in SKILL_FILES {
        let dest = target.join(relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create_dir_all {} failed: {e}", parent.display()))?;
        }
        if dest.exists() && !force {
            outcomes.push((*relative, InstallOutcome::Skipped));
            continue;
        }
        std::fs::write(&dest, body)
            .map_err(|e| format!("write {} failed: {e}", dest.display()))?;
        outcomes.push((*relative, InstallOutcome::Written));
    }
    Ok((outcomes, bytes_total, SKILL_FILES.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_target(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("collab-skills-{tag}-{}-{n}", std::process::id()))
    }

    #[test]
    fn install_writes_all_files_into_target() {
        let target = temp_target("write");
        let (outcomes, bytes, count) = install(&target, false).unwrap();
        assert_eq!(count, SKILL_FILES.len());
        assert_eq!(bytes, SKILL_FILES.iter().map(|(_, b)| b.len()).sum::<usize>());
        for (rel, outcome) in &outcomes {
            assert_eq!(outcome, &InstallOutcome::Written, "expected written: {rel}");
            assert!(target.join(rel).is_file(), "missing: {rel}");
        }
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn second_install_without_force_skips_existing() {
        let target = temp_target("skip");
        install(&target, false).unwrap();
        let (outcomes, _, _) = install(&target, false).unwrap();
        assert!(outcomes
            .iter()
            .all(|(_, o)| *o == InstallOutcome::Skipped));
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn force_overwrites_existing() {
        let target = temp_target("force");
        install(&target, false).unwrap();
        let first = std::fs::read_to_string(target.join("SKILL.md")).unwrap();
        std::fs::write(target.join("SKILL.md"), "tampered").unwrap();
        let (outcomes, _, _) = install(&target, true).unwrap();
        assert!(outcomes
            .iter()
            .all(|(_, o)| *o == InstallOutcome::Written));
        let restored = std::fs::read_to_string(target.join("SKILL.md")).unwrap();
        assert_eq!(restored, first);
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn empty_target_path_is_rejected() {
        let err = install(std::path::Path::new(""), false).unwrap_err();
        assert!(err.contains("non-empty path"), "{err}");
    }

    #[test]
    fn embedded_content_matches_repo_source_of_truth() {
        for (rel, embedded) in SKILL_FILES {
            let on_disk = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(SKILL_ROOT)
                    .join(rel),
            )
            .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            assert_eq!(on_disk, *embedded, "drift in {rel}");
        }
    }
}
