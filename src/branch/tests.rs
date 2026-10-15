use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use git2::{Oid, Repository, RepositoryInitOptions};

use super::{Choice, Operation, Progress, execute, git, snapshot};
use crate::status::{Change, Node};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ez-git-branch-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = Repository::init_opts(&path, &opts).unwrap();
        configure(&repo);
        Self(path)
    }

    fn repo(&self) -> Repository {
        Repository::open(&self.0).unwrap()
    }

    fn commit(&self, file: &str, text: &str, message: &str) -> Oid {
        fs::write(self.0.join(file), text).unwrap();
        self.commit_worktree(message)
    }

    /// 删除文件后提交，用来造「修改/删除」冲突。
    fn remove(&self, file: &str, message: &str) -> Oid {
        fs::remove_file(self.0.join(file)).unwrap();
        self.commit_worktree(message)
    }

    fn commit_worktree(&self, message: &str) -> Oid {
        let repo = self.repo();
        crate::commit::git_all(&repo).unwrap();
        crate::commit::git_commit(
            &repo,
            message,
            &git2::Signature::now("Test", "test@example.com").unwrap(),
        )
        .unwrap()
    }

    fn run(&self, operation: Operation) -> anyhow::Result<String> {
        execute(&self.0, &operation, &snapshot(&self.0)?)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn history_includes_branch_tips_and_merge_parents() {
    let fixture = Fixture::new();
    let root = fixture.commit("base", "base", "root");
    let repo = fixture.repo();
    repo.branch("side", &repo.find_commit(root).unwrap(), false)
        .unwrap();
    let main = fixture.commit("main", "main", "main commit");
    fixture
        .run(Operation::Switch("refs/heads/side".into()))
        .unwrap();
    let side = fixture.commit("side", "side", "side commit");
    fixture
        .run(Operation::Switch("refs/heads/main".into()))
        .unwrap();
    fixture
        .run(Operation::Merge("refs/heads/side".into()))
        .unwrap();
    let state = snapshot(&fixture.0).unwrap();
    let merge = &state.history[0];
    assert_eq!(Some(merge.oid), state.head);
    assert_eq!(merge.parents, vec![main, side]);
    for (index, node) in state.history.iter().enumerate() {
        for parent in &node.parents {
            let parent_index = state.history.iter().position(|n| n.oid == *parent).unwrap();
            assert!(parent_index > index);
        }
    }
    assert_eq!(state.history.last().unwrap().oid, root);
}

#[test]
fn history_handles_unborn_and_detached_head() {
    let fixture = Fixture::new();
    assert!(snapshot(&fixture.0).unwrap().history.is_empty());
    let oid = fixture.commit("base", "base", "root");
    fixture.repo().set_head_detached(oid).unwrap();
    let state = snapshot(&fixture.0).unwrap();
    assert_eq!(state.head, Some(oid));
    assert_eq!(state.history.len(), 1);
    assert_eq!(state.history[0].oid, oid);
}

fn configure(repo: &Repository) {
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Test").unwrap();
    config.set_str("user.email", "test@example.com").unwrap();
    config.set_bool("commit.gpgSign", false).unwrap();
    config
        .set_str(
            "core.hooksPath",
            repo.path().join("no-hooks").to_str().unwrap(),
        )
        .unwrap();
}

fn local(name: &str) -> String {
    format!("refs/heads/{name}")
}

/// 挑一侧解决当前冲突文件。
fn resolve(file: &str, side: Choice) -> Operation {
    Operation::Resolve {
        file: file.into(),
        side,
    }
}
#[test]
fn create_switch_and_dirty_guard_preserve_worktree() {
    let fixture = Fixture::new();
    let base = fixture.commit("a.txt", "base\n", "base");
    fixture.run(Operation::Create("feature/ui".into())).unwrap();
    let state = snapshot(&fixture.0).unwrap();
    assert!(
        state
            .branches
            .iter()
            .any(|branch| branch.name == "feature/ui")
    );
    assert_eq!(state.current.as_deref(), Some("main")); // 新建不暗中切换
    fixture.run(Operation::Switch(local("feature/ui"))).unwrap();
    assert_eq!(
        snapshot(&fixture.0).unwrap().current.as_deref(),
        Some("feature/ui")
    );
    fs::write(fixture.0.join("a.txt"), "unsaved\n").unwrap();
    for operation in [
        Operation::Switch(local("main")),
        Operation::Merge(local("main")),
        Operation::Rebase(local("main")),
        Operation::Pull,
    ] {
        assert!(
            fixture
                .run(operation)
                .unwrap_err()
                .to_string()
                .contains("未提交")
        );
    }
    assert_eq!(
        fs::read_to_string(fixture.0.join("a.txt")).unwrap(),
        "unsaved\n"
    );
    assert_eq!(fixture.repo().head().unwrap().target(), Some(base));
}

#[test]
fn invalid_unborn_and_stale_operations_are_rejected() {
    let fixture = Fixture::new();
    assert!(fixture.run(Operation::Create("feature".into())).is_err());
    fixture.commit("a.txt", "base\n", "base");
    for name in ["--force", "../bad", "bad name", "x\nmain", "@{oops}"] {
        assert!(fixture.run(Operation::Create(name.into())).is_err());
    }
    fixture.run(Operation::Create("feature".into())).unwrap();
    assert!(fixture.run(Operation::Create("feature".into())).is_err());
    let stale = snapshot(&fixture.0).unwrap();
    fixture.run(Operation::Switch(local("feature"))).unwrap();
    // 两个分支指向同一个提交，仍然要识别分支已经换了。
    assert!(execute(&fixture.0, &Operation::Merge(local("main")), &stale).is_err());
    git(&fixture.0, &["switch", "--detach"]).unwrap();
    assert!(fixture.run(Operation::Rebase(local("main"))).is_err());
}

#[test]
fn merge_supports_fast_forward_and_two_parent_commit() {
    let fixture = Fixture::new();
    fixture.commit("base.txt", "base\n", "base");
    fixture.run(Operation::Create("feature".into())).unwrap();
    fixture.run(Operation::Switch(local("feature"))).unwrap();
    let feature = fixture.commit("feature.txt", "feature\n", "feature");
    fixture.run(Operation::Switch(local("main"))).unwrap();
    fixture.run(Operation::Merge(local("feature"))).unwrap();
    assert_eq!(fixture.repo().head().unwrap().target(), Some(feature));
    fixture.commit("main.txt", "main\n", "main");
    fixture.run(Operation::Switch(local("feature"))).unwrap();
    fixture.commit("more.txt", "more\n", "more");
    fixture.run(Operation::Switch(local("main"))).unwrap();
    fixture.run(Operation::Merge(local("feature"))).unwrap();
    let repo = fixture.repo();
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(commit.parent_count(), 2);
    assert_eq!(snapshot(&fixture.0).unwrap().progress, Progress::Idle);
}

fn conflicting_branches(fixture: &Fixture) -> (Oid, Oid) {
    fixture.commit("conflict.txt", "base\n", "base");
    fixture.run(Operation::Create("feature".into())).unwrap();
    fixture.run(Operation::Switch(local("feature"))).unwrap();
    let feature = fixture.commit("conflict.txt", "feature\n", "feature");
    fixture.run(Operation::Switch(local("main"))).unwrap();
    let main = fixture.commit("conflict.txt", "main\n", "main");
    (main, feature)
}

#[test]
fn merge_conflict_can_be_resolved_and_continued() {
    let fixture = Fixture::new();
    let (main, feature) = conflicting_branches(&fixture);
    assert!(fixture.run(Operation::Merge(local("feature"))).is_err());
    let state = snapshot(&fixture.0).unwrap();
    assert_eq!(state.progress, Progress::Merge);
    assert_eq!(state.conflicts, ["conflict.txt"]);
    assert!(fixture.run(Operation::Continue).is_err());
    assert!(fixture.run(Operation::Switch(local("feature"))).is_err());
    fs::write(fixture.0.join("conflict.txt"), "resolved\n").unwrap();
    fixture
        .run(resolve("conflict.txt", Choice::Manual))
        .unwrap();
    fixture.run(Operation::Continue).unwrap();
    let repo = fixture.repo();
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(
        (commit.parent_id(0).unwrap(), commit.parent_id(1).unwrap()),
        (main, feature)
    );
    assert_eq!(snapshot(&fixture.0).unwrap().progress, Progress::Idle);
}

#[test]
fn merge_abort_restores_original_head_and_content() {
    let fixture = Fixture::new();
    let (main, _) = conflicting_branches(&fixture);
    assert!(fixture.run(Operation::Merge(local("feature"))).is_err());
    // 恢复操作不能依赖提交身份：配置文件损坏时也必须能退出冲突状态。
    fs::write(fixture.repo().commondir().join("ez-git.toml"), "broken = [").unwrap();
    fixture.run(Operation::Abort).unwrap();
    assert_eq!(fixture.repo().head().unwrap().target(), Some(main));
    assert_eq!(
        fs::read_to_string(fixture.0.join("conflict.txt")).unwrap(),
        "main\n"
    );
    assert_eq!(snapshot(&fixture.0).unwrap().progress, Progress::Idle);
}

#[test]
fn rebase_replays_current_branch_on_selected_target() {
    let fixture = Fixture::new();
    fixture.commit("base.txt", "base\n", "base");
    fixture.run(Operation::Create("feature".into())).unwrap();
    let main = fixture.commit("main.txt", "main\n", "main");
    fixture.run(Operation::Switch(local("feature"))).unwrap();
    let old = fixture.commit("feature.txt", "feature\n", "feature");
    fixture.run(Operation::Rebase(local("main"))).unwrap();
    let repo = fixture.repo();
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_ne!(commit.id(), old);
    assert_eq!(commit.parent_id(0).unwrap(), main);
    assert_eq!(
        snapshot(&fixture.0).unwrap().current.as_deref(),
        Some("feature")
    );
    assert!(fixture.0.join("main.txt").is_file());
    assert!(fixture.0.join("feature.txt").is_file());
}

#[test]
fn rebase_conflict_supports_continue_and_abort() {
    let fixture = Fixture::new();
    let (main, feature) = conflicting_branches(&fixture);
    fixture.run(Operation::Switch(local("feature"))).unwrap();
    assert!(fixture.run(Operation::Rebase(local("main"))).is_err());
    assert_eq!(snapshot(&fixture.0).unwrap().progress, Progress::Rebase);
    fixture.run(Operation::Abort).unwrap();
    assert_eq!(fixture.repo().head().unwrap().target(), Some(feature));
    assert_eq!(
        fs::read_to_string(fixture.0.join("conflict.txt")).unwrap(),
        "feature\n"
    );
    assert!(fixture.run(Operation::Rebase(local("main"))).is_err());
    fs::write(fixture.0.join("conflict.txt"), "resolved\n").unwrap();
    fixture
        .run(resolve("conflict.txt", Choice::Manual))
        .unwrap();
    fixture.run(Operation::Continue).unwrap();
    let repo = fixture.repo();
    assert_eq!(
        repo.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .parent_id(0)
            .unwrap(),
        main
    );
    assert_eq!(snapshot(&fixture.0).unwrap().progress, Progress::Idle);
    assert_eq!(
        snapshot(&fixture.0).unwrap().current.as_deref(),
        Some("feature")
    );
}

#[test]
fn rebase_skip_keeps_later_commits() {
    let fixture = Fixture::new();
    let (main, _) = conflicting_branches(&fixture);
    fixture.run(Operation::Switch(local("feature"))).unwrap();
    fixture.commit("later.txt", "later\n", "later");
    assert!(fixture.run(Operation::Rebase(local("main"))).is_err());
    fixture.run(Operation::Skip).unwrap();
    assert_eq!(snapshot(&fixture.0).unwrap().progress, Progress::Idle);
    assert_eq!(
        fs::read_to_string(fixture.0.join("conflict.txt")).unwrap(),
        "main\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("later.txt")).unwrap(),
        "later\n"
    );
    let repo = fixture.repo();
    assert_eq!(
        repo.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .parent_id(0)
            .unwrap(),
        main
    );
}

/// 冲突文件在状态树里单独一档，diff 面板也仍然读得到带冲突标记的内容。
#[test]
fn conflicted_file_is_marked_and_readable_in_the_main_window() {
    let fixture = Fixture::new();
    conflicting_branches(&fixture);
    assert!(fixture.run(Operation::Merge(local("feature"))).is_err());
    let repo = fixture.repo();

    let change = crate::status::tree(&repo)
        .unwrap()
        .into_iter()
        .find_map(|node| match node {
            Node::File { path, change, .. } if path == "conflict.txt" => Some(change),
            _ => None,
        });
    assert_eq!(change, Some(Change::Conflicted));

    let text = crate::status::git_file_diff(&repo, "conflict.txt")
        .unwrap()
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("<<<<<<<"), "{text}");
    assert!(text.contains(">>>>>>>"), "{text}");
}

#[test]
fn merge_conflict_can_take_either_side() {
    let fixture = Fixture::new();
    conflicting_branches(&fixture);
    assert!(fixture.run(Operation::Merge(local("feature"))).is_err());
    fixture.run(resolve("conflict.txt", Choice::Ours)).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.0.join("conflict.txt")).unwrap(),
        "main\n"
    );
    assert!(snapshot(&fixture.0).unwrap().conflicts.is_empty());
    fixture.run(Operation::Abort).unwrap();

    assert!(fixture.run(Operation::Merge(local("feature"))).is_err());
    fixture
        .run(resolve("conflict.txt", Choice::Theirs))
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.0.join("conflict.txt")).unwrap(),
        "feature\n"
    );
    fixture.run(Operation::Continue).unwrap();
    assert_eq!(snapshot(&fixture.0).unwrap().progress, Progress::Idle);
}

#[test]
fn rebase_conflict_side_follows_rebase_ours_and_theirs() {
    let fixture = Fixture::new();
    conflicting_branches(&fixture);
    fixture.run(Operation::Switch(local("feature"))).unwrap();
    assert!(fixture.run(Operation::Rebase(local("main"))).is_err());
    // 变基时 ours 是目标分支（main），theirs 是被重放的提交（feature）。
    fixture.run(resolve("conflict.txt", Choice::Ours)).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.0.join("conflict.txt")).unwrap(),
        "main\n"
    );
    fixture.run(Operation::Abort).unwrap();

    assert!(fixture.run(Operation::Rebase(local("main"))).is_err());
    fixture
        .run(resolve("conflict.txt", Choice::Theirs))
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.0.join("conflict.txt")).unwrap(),
        "feature\n"
    );
    fixture.run(Operation::Continue).unwrap();
    assert_eq!(snapshot(&fixture.0).unwrap().progress, Progress::Idle);
}

/// 修改/删除冲突里被删的那一侧在索引里没有条目，取它就是把删除落进索引。
#[test]
fn modify_delete_conflict_can_keep_the_deletion() {
    let fixture = Fixture::new();
    fixture.commit("gone.txt", "base\n", "base");
    fixture.run(Operation::Create("feature".into())).unwrap();
    fixture.run(Operation::Switch(local("feature"))).unwrap();
    fixture.commit("gone.txt", "feature\n", "feature");
    fixture.run(Operation::Switch(local("main"))).unwrap();
    fixture.remove("gone.txt", "delete on main");

    assert!(fixture.run(Operation::Merge(local("feature"))).is_err());
    assert_eq!(snapshot(&fixture.0).unwrap().conflicts, ["gone.txt"]);
    fixture.run(resolve("gone.txt", Choice::Ours)).unwrap();
    assert!(!fixture.0.join("gone.txt").exists());
    assert!(snapshot(&fixture.0).unwrap().conflicts.is_empty());
    fixture.run(Operation::Abort).unwrap();
    // 中止后回到 main 的删除提交，文件仍然不存在。
    assert!(!fixture.0.join("gone.txt").exists());

    // 另一侧：feature 改了文件，取 theirs 把它拿回来。
    assert!(fixture.run(Operation::Merge(local("feature"))).is_err());
    fixture.run(resolve("gone.txt", Choice::Theirs)).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.0.join("gone.txt")).unwrap(),
        "feature\n"
    );
    fixture.run(Operation::Continue).unwrap();
    assert_eq!(snapshot(&fixture.0).unwrap().progress, Progress::Idle);
}

/// 冲突没解决时，状态树那条线不能把带 `<<<<<<<` 的内容当成「已解决」暂存。
#[test]
fn unresolved_conflicts_are_refused_by_the_commit_paths() {
    let fixture = Fixture::new();
    conflicting_branches(&fixture);
    assert!(fixture.run(Operation::Merge(local("feature"))).is_err());
    let repo = fixture.repo();

    // `git add -A`（全部提交）和单个路径的勾选都要被拦下。
    assert!(crate::commit::git_all(&repo).is_err());
    assert!(crate::commit::git_stage(&repo, &["conflict.txt".into()]).is_err());
    // 其它文件照旧能暂存，只是不许碰冲突路径。
    fs::write(fixture.0.join("other.txt"), "other\n").unwrap();
    crate::commit::git_stage(&repo, &["other.txt".into()]).unwrap();
    assert_eq!(snapshot(&fixture.0).unwrap().conflicts, ["conflict.txt"]);
}

fn run(path: &Path, operation: Operation) -> anyhow::Result<String> {
    execute(path, &operation, &snapshot(path)?)
}

#[test]
fn fetch_pull_upstream_and_divergence_work_without_network() {
    let fixture = Fixture::new();
    fixture.commit("base.txt", "base\n", "base");
    let peers = Fixture::new();
    let remote_path = peers.0.join("remote.git");
    Repository::init_bare(&remote_path).unwrap();
    fixture
        .repo()
        .remote("origin", remote_path.to_str().unwrap())
        .unwrap();
    crate::sync::git_push(&fixture.repo()).unwrap();
    let clone_path = peers.0.join("clone");
    // 裸仓库 HEAD 需要指向实际被推送的 main，避免 clone 一个空的 master。
    Repository::open_bare(&remote_path)
        .unwrap()
        .set_head(&local("main"))
        .unwrap();
    let clone = Repository::clone(remote_path.to_str().unwrap(), &clone_path).unwrap();
    configure(&clone);
    let source = fixture.commit("remote.txt", "remote\n", "remote");
    crate::sync::git_push(&fixture.repo()).unwrap();
    let before = clone.head().unwrap().target();
    fs::write(clone_path.join("draft.txt"), "not committed\n").unwrap();
    run(&clone_path, Operation::Fetch("origin".into())).unwrap();
    assert_eq!(clone.head().unwrap().target(), before); // Fetch 不动当前分支/文件
    assert!(!clone_path.join("remote.txt").exists());
    assert_eq!(
        fs::read_to_string(clone_path.join("draft.txt")).unwrap(),
        "not committed\n"
    );
    fs::remove_file(clone_path.join("draft.txt")).unwrap();
    run(&clone_path, Operation::Pull).unwrap();
    assert_eq!(clone.head().unwrap().target(), Some(source));
    assert!(clone_path.join("remote.txt").exists());
    run(&clone_path, Operation::Create("topic".into())).unwrap();
    run(&clone_path, Operation::Switch(local("topic"))).unwrap();
    assert!(run(&clone_path, Operation::Pull).is_err());
    run(
        &clone_path,
        Operation::SetUpstream("refs/remotes/origin/main".into()),
    )
    .unwrap();
    assert!(
        snapshot(&clone_path)
            .unwrap()
            .branches
            .iter()
            .any(|branch| branch.name == "topic"
                && branch.upstream.as_deref() == Some("origin/main"))
    );
    fs::write(clone_path.join("local.txt"), "local\n").unwrap();
    crate::commit::git_all(&clone).unwrap();
    crate::commit::git_commit(
        &clone,
        "local",
        &git2::Signature::now("Test", "test@example.com").unwrap(),
    )
    .unwrap();
    fixture.commit("remote2.txt", "remote2\n", "remote2");
    crate::sync::git_push(&fixture.repo()).unwrap();
    let local_head = clone.head().unwrap().target();
    assert!(run(&clone_path, Operation::Pull).is_err()); // 不暗中整合分叉
    assert_eq!(clone.head().unwrap().target(), local_head);
    run(
        &clone_path,
        Operation::Merge("refs/remotes/origin/main".into()),
    )
    .unwrap();
    assert_eq!(
        clone
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .parent_count(),
        2
    );
}

fn automatic(path: &Path) -> anyhow::Result<super::AutoPullResult> {
    // 其他分支测试并行运行，等待锁后只验证自动任务的仓库安全策略。
    let _guard = super::GIT_OPERATIONS.lock().unwrap();
    super::auto_pull_locked(path)
}

#[test]
fn auto_pull_settings_survive_identity_changes_and_roundtrip() {
    use crate::config::{AutoPullConfig, RepoConfig};

    let fixture = Fixture::new();
    RepoConfig::save_identity(&fixture.0, "First".into(), "first@example.com".into()).unwrap();
    let settings = AutoPullConfig {
        enabled: true,
        interval_secs: 42,
    };
    RepoConfig::save_auto_pull(&fixture.0, settings).unwrap();
    let loaded = RepoConfig::load(&fixture.0).unwrap();
    assert_eq!(loaded.author, "First");
    assert_eq!(loaded.auto_pull, settings);
    RepoConfig::save_identity(&fixture.0, "Second".into(), "second@example.com".into()).unwrap();
    assert_eq!(RepoConfig::load(&fixture.0).unwrap().auto_pull, settings);
    RepoConfig::save_auto_pull(
        &fixture.0,
        AutoPullConfig {
            enabled: false,
            ..settings
        },
    )
    .unwrap();
    let disabled = RepoConfig::load(&fixture.0).unwrap();
    assert!(!disabled.auto_pull.enabled);
    assert_eq!(disabled.auto_pull.interval().unwrap(), 42);
    assert_eq!(disabled.author, "Second");
    assert_eq!(disabled.email, "second@example.com");
}

#[test]
fn auto_pull_skips_when_another_branch_operation_is_running() {
    let _guard = super::GIT_OPERATIONS.lock().unwrap();
    assert!(matches!(
        super::try_auto_pull(Path::new("not-accessed")).unwrap(),
        super::AutoPullResult::Skipped(_)
    ));
}

#[test]
fn auto_pull_skips_dirty_conflicted_unborn_detached_and_missing_upstream() {
    use super::AutoPullResult;

    let fixture = Fixture::new();
    assert!(
        matches!(automatic(&fixture.0).unwrap(), AutoPullResult::Skipped(reason) if reason.contains("没有提交"))
    );
    fixture.commit("a.txt", "base\n", "base");
    assert!(
        matches!(automatic(&fixture.0).unwrap(), AutoPullResult::Skipped(reason) if reason.contains("上游"))
    );
    let before = fixture.repo().head().unwrap().target();
    fs::write(fixture.0.join("a.txt"), "draft\n").unwrap();
    assert!(
        matches!(automatic(&fixture.0).unwrap(), AutoPullResult::Skipped(reason) if reason.contains("未提交"))
    );
    crate::commit::git_stage(&fixture.repo(), &["a.txt".into()]).unwrap();
    assert!(
        matches!(automatic(&fixture.0).unwrap(), AutoPullResult::Skipped(reason) if reason.contains("未提交"))
    );
    assert_eq!(fixture.repo().head().unwrap().target(), before);
    assert_eq!(
        fs::read_to_string(fixture.0.join("a.txt")).unwrap(),
        "draft\n"
    );
    crate::commit::git_unstage(&fixture.repo(), &["a.txt".into()]).unwrap();
    fs::write(fixture.0.join("a.txt"), "base\n").unwrap();
    fs::write(fixture.0.join("untracked.txt"), "keep\n").unwrap();
    assert!(
        matches!(automatic(&fixture.0).unwrap(), AutoPullResult::Skipped(reason) if reason.contains("未提交"))
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("untracked.txt")).unwrap(),
        "keep\n"
    );
    fs::remove_file(fixture.0.join("untracked.txt")).unwrap();
    conflicting_branches(&fixture);
    assert!(fixture.run(Operation::Merge(local("feature"))).is_err());
    assert!(
        matches!(automatic(&fixture.0).unwrap(), AutoPullResult::Skipped(reason) if reason.contains("正在进行"))
    );
    fixture.run(Operation::Abort).unwrap();
    git(&fixture.0, &["switch", "--detach"]).unwrap();
    assert!(
        matches!(automatic(&fixture.0).unwrap(), AutoPullResult::Skipped(reason) if reason.contains("detached"))
    );
}

#[test]
fn auto_pull_fast_forwards_but_never_merges_divergence() {
    let source = Fixture::new();
    source.commit("base.txt", "base\n", "base");
    let peers = Fixture::new();
    let path = peers.0.join("clone");
    let clone = Repository::clone(source.0.to_str().unwrap(), &path).unwrap();
    configure(&clone);
    let remote_head = source.commit("remote.txt", "remote\n", "remote");
    assert!(matches!(
        automatic(&path).unwrap(),
        super::AutoPullResult::Pulled(_)
    ));
    assert_eq!(clone.head().unwrap().target(), Some(remote_head));
    assert_eq!(
        fs::read_to_string(path.join("remote.txt")).unwrap(),
        "remote\n"
    );
    fs::write(path.join("local.txt"), "local\n").unwrap();
    crate::commit::git_all(&clone).unwrap();
    crate::commit::git_commit(
        &clone,
        "local",
        &git2::Signature::now("Test", "test@example.com").unwrap(),
    )
    .unwrap();
    source.commit("remote2.txt", "remote2\n", "remote2");
    let local_head = clone.head().unwrap().target();
    assert!(automatic(&path).is_err());
    assert_eq!(clone.head().unwrap().target(), local_head);
    assert!(!path.join("remote2.txt").exists());
    assert_eq!(
        fs::read_to_string(path.join("local.txt")).unwrap(),
        "local\n"
    );
}

#[test]
fn timer_config_reads_never_see_a_partial_save() {
    use crate::config::{AutoPullConfig, RepoConfig, config_path};

    let fixture = Fixture::new();
    RepoConfig::save_auto_pull(
        &fixture.0,
        AutoPullConfig {
            enabled: true,
            interval_secs: 10,
        },
    )
    .unwrap();
    let file = config_path(&fixture.0).unwrap();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            barrier.wait();
            for _ in 0..2000 {
                let text = fs::read_to_string(&file).unwrap();
                let config: RepoConfig = toml::from_str(&text).unwrap();
                assert!(config.auto_pull.enabled);
                assert!((10..=109).contains(&config.auto_pull.interval().unwrap()));
            }
        });
        barrier.wait();
        for seconds in 10..110 {
            RepoConfig::save_auto_pull(
                &fixture.0,
                AutoPullConfig {
                    enabled: true,
                    interval_secs: seconds,
                },
            )
            .unwrap();
        }
        reader.join().unwrap();
    });
    assert!(
        !file
            .with_extension(format!("toml.tmp-{}", std::process::id()))
            .exists()
    );
}
