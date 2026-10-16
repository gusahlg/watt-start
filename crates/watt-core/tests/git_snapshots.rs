//! Integration tests for `git` and `snapshots`: local source repositories in a tempdir stand in
//! for the public mirrors, so every test runs offline and fast.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicI64, Ordering};

use watt_core::git;
use watt_core::paths::{Paths, Repo};
use watt_core::snapshots;

/// Commits in these tests can happen faster than the 1-second resolution of commit timestamps;
/// giving each one an explicit, strictly increasing time keeps `--sort=-committerdate` and
/// similar order-dependent assertions deterministic.
static CLOCK: AtomicI64 = AtomicI64::new(1_700_000_000);

/// Run `git` in `dir`, panicking with its stderr on failure.
fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    run_git(dir, &["init", "-q", "-b", "main"]);
}

/// Write `file`, commit it, return the new commit's full hash. Each commit gets a strictly later
/// timestamp than the previous one (see [`CLOCK`]).
fn commit(dir: &Path, file: &str, contents: &str, message: &str) -> String {
    std::fs::write(dir.join(file), contents).unwrap();
    run_git(dir, &["add", file]);
    let date = format!("{} +0000", CLOCK.fetch_add(60, Ordering::SeqCst));
    let output = Command::new("git")
        .args(["commit", "-q", "-m", message])
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git commit in {} failed:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    run_git(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn paths_in(tmp: &Path) -> Paths {
    let dirs = pwc_instance::Dirs::under(tmp);
    Paths::new(dirs).expect("paths")
}

fn no_progress() -> (impl FnMut(git::GitProgress), impl FnMut(String)) {
    (|_p: git::GitProgress| {}, |_l: String| {})
}

#[test]
fn version_reports_something() {
    let out = git::version().expect("git must be installed to run these tests");
    assert!(out.to_lowercase().contains("git"), "{out}");
}

#[test]
fn ensure_mirror_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    commit(&src, "a.txt", "1", "init");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();

    let (mut p1, mut l1) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p1, &mut l1).unwrap();
    assert!(mirror.is_dir());
    assert!(git::objects_dir(&mirror).is_dir());

    // Second call with the same remote is a no-op: no clone, no error.
    let (mut p2, mut l2) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p2, &mut l2).unwrap();
    assert_eq!(git::default_branch(&mirror).unwrap(), "main");
}

#[test]
fn ensure_mirror_updates_origin_url_when_it_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    commit(&src, "a.txt", "1", "init");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();

    let other = tmp.path().join("other");
    init_repo(&other);
    commit(&other, "b.txt", "1", "init");
    let other_remote = other.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &other_remote, &mut p, &mut l).unwrap();

    let current = run_git(&mirror, &["config", "--get", "remote.origin.url"]);
    assert_eq!(current.trim(), other_remote);
}

#[test]
fn fetch_detects_new_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    commit(&src, "a.txt", "1", "init");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();

    // Nothing changed yet.
    let (mut p, mut l) = no_progress();
    assert!(!git::fetch(&mirror, &mut p, &mut l).unwrap());

    commit(&src, "a.txt", "2", "second");
    let (mut p, mut l) = no_progress();
    assert!(git::fetch(&mirror, &mut p, &mut l).unwrap());
}

#[test]
fn resolve_branch_tag_hash_and_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    let sha1 = commit(&src, "a.txt", "1", "init");
    run_git(&src, &["tag", "v1"]);
    let sha2 = commit(&src, "a.txt", "2", "second");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();
    let (mut p, mut l) = no_progress();
    git::fetch(&mirror, &mut p, &mut l).unwrap();

    assert_eq!(git::resolve(&mirror, "main").unwrap(), Some(sha2.clone()));
    assert_eq!(git::resolve(&mirror, "v1").unwrap(), Some(sha1.clone()));
    assert_eq!(
        git::resolve(&mirror, &sha2[..10]).unwrap(),
        Some(sha2.clone())
    );
    assert_eq!(git::resolve(&mirror, &sha1).unwrap(), Some(sha1.clone()));
    assert_eq!(git::resolve(&mirror, "no-such-ref").unwrap(), None);
}

#[test]
fn resolve_prefers_branch_over_same_named_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    let sha1 = commit(&src, "a.txt", "1", "init");
    run_git(&src, &["branch", "release"]);
    let sha2 = commit(&src, "a.txt", "2", "second");
    // A tag with the same name as the branch, pointing elsewhere.
    run_git(&src, &["tag", "release", &sha1]);

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();
    let (mut p, mut l) = no_progress();
    git::fetch(&mirror, &mut p, &mut l).unwrap();

    // `release` the branch (sha1 is `release`'s tip, created before the second commit).
    assert_eq!(git::resolve(&mirror, "release").unwrap(), Some(sha1));
    let _ = sha2;
}

#[test]
fn default_branch_reads_head() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    commit(&src, "a.txt", "1", "init");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();

    assert_eq!(git::default_branch(&mirror).unwrap(), "main");
}

#[test]
fn branches_are_sorted_newest_tip_first() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    commit(&src, "a.txt", "1", "init");
    run_git(&src, &["checkout", "-q", "-b", "old"]);
    commit(&src, "a.txt", "2", "old work");
    run_git(&src, &["checkout", "-q", "main"]);
    run_git(&src, &["checkout", "-q", "-b", "new"]);
    let newest = commit(&src, "a.txt", "3", "new work");
    run_git(&src, &["checkout", "-q", "main"]);

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();
    let (mut p, mut l) = no_progress();
    git::fetch(&mirror, &mut p, &mut l).unwrap();

    let branches = git::branches(&mirror).unwrap();
    let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names[0], "new", "{names:?}");
    let new_branch = branches.iter().find(|b| b.name == "new").unwrap();
    assert_eq!(new_branch.sha, newest);
    assert_eq!(new_branch.summary, "new work");
}

#[test]
fn tags_lists_names() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    commit(&src, "a.txt", "1", "init");
    run_git(&src, &["tag", "v1"]);
    commit(&src, "a.txt", "2", "second");
    run_git(&src, &["tag", "v2"]);

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();
    let (mut p, mut l) = no_progress();
    git::fetch(&mirror, &mut p, &mut l).unwrap();

    let tags = git::tags(&mirror).unwrap();
    assert_eq!(tags.len(), 2);
    assert!(tags.contains(&"v1".to_owned()));
    assert!(tags.contains(&"v2".to_owned()));
}

#[test]
fn log_between_orders_newest_first_and_truncates() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    let base = commit(&src, "a.txt", "0", "base");
    let c1 = commit(&src, "a.txt", "1", "one");
    let c2 = commit(&src, "a.txt", "2", "two");
    let c3 = commit(&src, "a.txt", "3", "three");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();
    let (mut p, mut l) = no_progress();
    git::fetch(&mirror, &mut p, &mut l).unwrap();

    let log = git::log_between(Repo::Game, &mirror, Some(&base), &c3, 10).unwrap();
    assert!(!log.truncated);
    let shas: Vec<&str> = log.commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(shas, [c3.as_str(), c2.as_str(), c1.as_str()]);
    assert_eq!(log.commits[0].summary, "three");
    assert_eq!(log.commits[0].author, "Test");

    let truncated = git::log_between(Repo::Game, &mirror, Some(&base), &c3, 2).unwrap();
    assert!(truncated.truncated);
    assert_eq!(truncated.commits.len(), 2);
    let shas: Vec<&str> = truncated.commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(shas, [c3.as_str(), c2.as_str()]);

    // `old = None` lists the newest commits of `new`.
    let no_old = git::log_between(Repo::Game, &mirror, None, &c3, 2).unwrap();
    assert!(no_old.truncated);
    let shas: Vec<&str> = no_old.commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(shas, [c3.as_str(), c2.as_str()]);
}

#[test]
fn file_at_present_missing_and_unknown_sha() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    let sha = commit(&src, "a.txt", "hello", "init");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();
    let (mut p, mut l) = no_progress();
    git::fetch(&mirror, &mut p, &mut l).unwrap();

    assert_eq!(
        git::file_at(&mirror, &sha, "a.txt").unwrap(),
        Some("hello".to_owned())
    );
    assert_eq!(git::file_at(&mirror, &sha, "nope.txt").unwrap(), None);
    assert!(git::file_at(&mirror, &"f".repeat(40), "a.txt").is_err());
}

#[test]
fn pin_survives_a_force_push_and_prune() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    let base = commit(&src, "a.txt", "0", "base");
    let doomed = commit(&src, "a.txt", "1", "doomed");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();
    let (mut p, mut l) = no_progress();
    git::fetch(&mirror, &mut p, &mut l).unwrap();

    git::pin(&mirror, &doomed).unwrap();

    // Force the source branch back to `base` and lose `doomed` from `main`.
    run_git(&src, &["reset", "-q", "--hard", &base]);
    let (mut p, mut l) = no_progress();
    git::fetch(&mirror, &mut p, &mut l).unwrap();

    // `main` moved, but the pin kept `doomed` resolvable.
    assert_eq!(git::resolve(&mirror, "main").unwrap(), Some(base));
    assert_eq!(
        git::resolve(&mirror, &doomed).unwrap(),
        Some(doomed.clone())
    );
    let refs = run_git(
        &mirror,
        &["for-each-ref", "--format=%(refname)", "refs/pins"],
    );
    assert!(refs.contains(&format!("refs/pins/{doomed}")), "{refs}");

    // And it is still clonable.
    let dest = tmp.path().join("dest");
    let (mut p, mut l) = no_progress();
    git::clone_at(&mirror, &doomed, &dest, &mut p, &mut l).unwrap();
    let (head, clean) = git::head_and_clean(&dest).unwrap();
    assert_eq!(head, doomed);
    assert!(clean);
}

#[test]
fn unpin_except_drops_unwanted_pins() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    let a = commit(&src, "a.txt", "1", "a");
    let b = commit(&src, "a.txt", "2", "b");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();
    let (mut p, mut l) = no_progress();
    git::fetch(&mirror, &mut p, &mut l).unwrap();

    git::pin(&mirror, &a).unwrap();
    git::pin(&mirror, &b).unwrap();

    let keep: BTreeSet<String> = [b.clone()].into_iter().collect();
    git::unpin_except(&mirror, &keep).unwrap();

    let refs = run_git(
        &mirror,
        &["for-each-ref", "--format=%(refname)", "refs/pins"],
    );
    assert!(!refs.contains(&format!("refs/pins/{a}")), "{refs}");
    assert!(refs.contains(&format!("refs/pins/{b}")), "{refs}");
}

#[test]
fn clone_at_checks_out_detached() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    init_repo(&src);
    let sha = commit(&src, "a.txt", "1", "init");

    let paths = paths_in(tmp.path());
    let mirror = paths.mirror(Repo::Game);
    let remote = src.to_string_lossy().into_owned();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(&mirror, &remote, &mut p, &mut l).unwrap();

    let dest = tmp.path().join("dest");
    let (mut p, mut l) = no_progress();
    git::clone_at(&mirror, &sha, &dest, &mut p, &mut l).unwrap();
    let (head, clean) = git::head_and_clean(&dest).unwrap();
    assert_eq!(head, sha);
    assert!(clean);

    // The destination must not already exist.
    let (mut p, mut l) = no_progress();
    assert!(git::clone_at(&mirror, &sha, &dest, &mut p, &mut l).is_err());
}

#[test]
fn ensure_game_creates_a_complete_snapshot_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let game_src = tmp.path().join("game");
    let engine_src = tmp.path().join("engine");
    init_repo(&game_src);
    init_repo(&engine_src);
    let game_sha = commit(&game_src, "a.txt", "1", "game init");
    let engine_sha = commit(&engine_src, "a.txt", "1", "engine init");

    let paths = paths_in(tmp.path());
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(
        &paths.mirror(Repo::Game),
        &game_src.to_string_lossy(),
        &mut p,
        &mut l,
    )
    .unwrap();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(
        &paths.mirror(Repo::Engine),
        &engine_src.to_string_lossy(),
        &mut p,
        &mut l,
    )
    .unwrap();

    let mut on_progress = |_repo: Repo, _p: git::GitProgress| {};
    let mut on_line = |_l: String| {};
    let snapshot = snapshots::ensure_game(
        &paths,
        &game_sha,
        &engine_sha,
        &mut on_progress,
        &mut on_line,
    )
    .unwrap();
    assert!(snapshots::is_complete(&snapshot.dir));
    let (head, clean) = git::head_and_clean(&snapshot.game).unwrap();
    assert_eq!(head, game_sha);
    assert!(clean);
    let (head, clean) = git::head_and_clean(&snapshot.engine).unwrap();
    assert_eq!(head, engine_sha);
    assert!(clean);

    // Idempotent: a second call does not fail and returns the same paths without recloning.
    let snapshot2 = snapshots::ensure_game(
        &paths,
        &game_sha,
        &engine_sha,
        &mut on_progress,
        &mut on_line,
    )
    .unwrap();
    assert_eq!(snapshot, snapshot2);
}

#[test]
fn ensure_game_replaces_a_leftover_incomplete_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let game_src = tmp.path().join("game");
    let engine_src = tmp.path().join("engine");
    init_repo(&game_src);
    init_repo(&engine_src);
    let game_sha = commit(&game_src, "a.txt", "1", "game init");
    let engine_sha = commit(&engine_src, "a.txt", "1", "engine init");

    let paths = paths_in(tmp.path());
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(
        &paths.mirror(Repo::Game),
        &game_src.to_string_lossy(),
        &mut p,
        &mut l,
    )
    .unwrap();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(
        &paths.mirror(Repo::Engine),
        &engine_src.to_string_lossy(),
        &mut p,
        &mut l,
    )
    .unwrap();

    let snapshot_dir = snapshots::game_snapshot(&paths, &game_sha, &engine_sha).dir;
    std::fs::create_dir_all(snapshot_dir.join("junk")).unwrap();
    std::fs::write(snapshot_dir.join("junk").join("x"), "garbage").unwrap();
    assert!(!snapshots::is_complete(&snapshot_dir));

    let mut on_progress = |_repo: Repo, _p: git::GitProgress| {};
    let mut on_line = |_l: String| {};
    let snapshot = snapshots::ensure_game(
        &paths,
        &game_sha,
        &engine_sha,
        &mut on_progress,
        &mut on_line,
    )
    .unwrap();
    assert!(snapshots::is_complete(&snapshot.dir));
    assert!(!snapshot.dir.join("junk").exists());
}

#[test]
fn ensure_game_recreates_a_modified_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let game_src = tmp.path().join("game");
    let engine_src = tmp.path().join("engine");
    init_repo(&game_src);
    init_repo(&engine_src);
    let game_sha = commit(&game_src, "a.txt", "1", "game init");
    let engine_sha = commit(&engine_src, "a.txt", "1", "engine init");

    let paths = paths_in(tmp.path());
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(
        &paths.mirror(Repo::Game),
        &game_src.to_string_lossy(),
        &mut p,
        &mut l,
    )
    .unwrap();
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(
        &paths.mirror(Repo::Engine),
        &engine_src.to_string_lossy(),
        &mut p,
        &mut l,
    )
    .unwrap();

    let mut on_progress = |_repo: Repo, _p: git::GitProgress| {};
    let mut on_line = |_l: String| {};
    let snapshot = snapshots::ensure_game(
        &paths,
        &game_sha,
        &engine_sha,
        &mut on_progress,
        &mut on_line,
    )
    .unwrap();

    // Dirty the game checkout: `ensure_game` must notice and recreate it.
    std::fs::write(snapshot.game.join("a.txt"), "tampered").unwrap();

    let snapshot2 = snapshots::ensure_game(
        &paths,
        &game_sha,
        &engine_sha,
        &mut on_progress,
        &mut on_line,
    )
    .unwrap();
    assert_eq!(snapshot, snapshot2);
    let (head, clean) = git::head_and_clean(&snapshot2.game).unwrap();
    assert_eq!(head, game_sha);
    assert!(clean);
    assert_eq!(
        std::fs::read_to_string(snapshot2.game.join("a.txt")).unwrap(),
        "1"
    );
}

#[test]
fn ensure_mods_creates_a_complete_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let mods_src = tmp.path().join("mods");
    init_repo(&mods_src);
    let mods_sha = commit(&mods_src, "a.txt", "1", "mods init");

    let paths = paths_in(tmp.path());
    let (mut p, mut l) = no_progress();
    git::ensure_mirror(
        &paths.mirror(Repo::Mods),
        &mods_src.to_string_lossy(),
        &mut p,
        &mut l,
    )
    .unwrap();

    let mut on_progress = |_repo: Repo, _p: git::GitProgress| {};
    let mut on_line = |_l: String| {};
    let dir = snapshots::ensure_mods(&paths, &mods_sha, &mut on_progress, &mut on_line).unwrap();
    assert_eq!(dir, snapshots::mods_snapshot(&paths, &mods_sha));
    assert!(snapshots::is_complete(&dir));
    let checkout = dir.join(Repo::Mods.dir_name());
    let (head, clean) = git::head_and_clean(&checkout).unwrap();
    assert_eq!(head, mods_sha);
    assert!(clean);

    // Idempotent.
    let dir2 = snapshots::ensure_mods(&paths, &mods_sha, &mut on_progress, &mut on_line).unwrap();
    assert_eq!(dir, dir2);
}

#[test]
fn list_reports_every_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let sources = paths.sources();
    std::fs::create_dir_all(sources.join("pwc-aaa-bbb")).unwrap();
    std::fs::write(sources.join("pwc-aaa-bbb").join("x"), "12345").unwrap();
    std::fs::write(sources.join("pwc-aaa-bbb").join(snapshots::COMPLETE), "").unwrap();
    std::fs::create_dir_all(sources.join("mods-ccc")).unwrap();
    // Hidden leftovers are not snapshots.
    std::fs::create_dir_all(sources.join(".tmp-xyz-1")).unwrap();

    let list = snapshots::list(&paths).unwrap();
    assert_eq!(list.len(), 2, "{list:?}");
    let complete = list
        .iter()
        .find(|s| s.dir.ends_with("pwc-aaa-bbb"))
        .unwrap();
    assert!(complete.complete);
    assert_eq!(complete.bytes, 5);
    let incomplete = list.iter().find(|s| s.dir.ends_with("mods-ccc")).unwrap();
    assert!(!incomplete.complete);
}

#[test]
fn gc_keeps_named_snapshots_and_deletes_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_in(tmp.path());
    let sources = paths.sources();
    let keep_dir = sources.join("pwc-keep");
    let drop_dir = sources.join("pwc-drop");
    std::fs::create_dir_all(&keep_dir).unwrap();
    std::fs::create_dir_all(&drop_dir).unwrap();
    std::fs::write(keep_dir.join(snapshots::COMPLETE), "").unwrap();
    std::fs::write(drop_dir.join(snapshots::COMPLETE), "").unwrap();

    let mut keep = BTreeSet::new();
    keep.insert(keep_dir.clone());
    // A path that does not exist must be tolerated.
    keep.insert(sources.join("does-not-exist"));

    let dry = snapshots::gc(&paths, &keep, true).unwrap();
    assert_eq!(dry, vec![drop_dir.clone()]);
    assert!(drop_dir.is_dir(), "dry run must not delete anything");
    assert!(keep_dir.is_dir());

    let deleted = snapshots::gc(&paths, &keep, false).unwrap();
    assert_eq!(deleted, vec![drop_dir.clone()]);
    assert!(!drop_dir.exists());
    assert!(keep_dir.is_dir());
}
