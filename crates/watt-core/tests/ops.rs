//! Integration tests for `ops::run`, `catalog` and `status`, against a fake PWC source, a fake
//! package repository and a fake toolchain (no real Cargo ever runs).

mod support;

use std::time::{Duration, Instant};

use support::World;
use watt_core::events::{Change, Event, OpOutcome, Recovery};
use watt_core::ops::{OpRequest, SourceSpec};
use watt_core::status::{self, Readiness};
use watt_core::{catalog, locks::FileLock, paths::InstancePaths};

fn create(world: &World, name: &str, source: SourceSpec, mods: &[&str], build: bool) -> OpOutcome {
    let (outcome, _sink) = world.run(&OpRequest::Create {
        name: name.to_owned(),
        display_name: None,
        color: None,
        group: None,
        source,
        mods: mods.iter().map(|m| m.to_owned().to_owned()).collect(),
        build,
    });
    outcome
}

fn assert_ok(outcome: &OpOutcome) -> &str {
    match outcome {
        OpOutcome::Ok { summary } => summary,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn assert_failed(outcome: &OpOutcome) -> (Option<&str>, &[Recovery]) {
    match outcome {
        OpOutcome::Failed {
            error, recovery, ..
        } => (Some(error.as_str()), recovery.as_slice()),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn create_pwc_default_and_build_then_cache_hits() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);

    let outcome = create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        true,
    );
    let summary = assert_ok(&outcome);
    assert!(summary.contains("Created"), "{summary}");
    assert_eq!(world.cargo_calls().len(), 1);

    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    assert_eq!(card.packages.len(), 1);
    assert_eq!(card.packages[0].id, "pwc.hotbar");
    assert_eq!(card.meta.generations.len(), 1);
    assert!(card.meta.generations[0].build.is_some());

    let status = status::compute(&world.paths, &card).unwrap();
    assert!(
        matches!(status.readiness, Readiness::Ready { .. }),
        "{:?}",
        status.readiness
    );
    assert!(!status.busy);
    assert!(status.running.is_none());

    // Second build: the cache answers, no second Cargo run.
    let (outcome, sink) = world.run(&OpRequest::Build {
        instance: "survival".to_owned(),
        force: false,
        profile: None,
    });
    let summary = assert_ok(&outcome);
    assert!(summary.contains("cached"), "{summary}");
    assert_eq!(world.cargo_calls().len(), 1);
    let cached = sink.0.lock().unwrap().iter().any(|e| {
        matches!(
            &e.event,
            Event::Changed {
                change: Change::Build { cached: true, .. }
            }
        )
    });
    assert!(cached, "expected a cached Change::Build event");
}

#[test]
fn create_linked_source_builds_too() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    let pwc_dir = world.pwc_dir.clone();

    let outcome = create(
        &world,
        "dev",
        SourceSpec::Linked {
            pwc_source: pwc_dir,
            repositories: Vec::new(),
        },
        &["pwc.hotbar"],
        true,
    );
    assert_ok(&outcome);
    let card = catalog::load_instance(&world.paths, "dev").unwrap();
    assert_eq!(card.packages.len(), 1);
    assert!(matches!(
        status::compute(&world.paths, &card).unwrap().readiness,
        Readiness::Ready { .. }
    ));
}

#[test]
fn mods_add_remove_relocks_and_restores_on_failure() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    world.add_package("pwc.inventory", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));

    // A good add/remove.
    let (outcome, _sink) = world.run(&OpRequest::Mods {
        instance: "survival".to_owned(),
        add: vec!["pwc.inventory".to_owned()],
        remove: vec!["pwc.hotbar".to_owned()],
        update: vec![],
        build: Some(false),
    });
    assert_ok(&outcome);
    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    assert_eq!(card.packages.len(), 1);
    assert_eq!(card.packages[0].id, "pwc.inventory");

    // Adding an id nothing provides fails at lock time; instance.toml is restored.
    let (outcome, _sink) = world.run(&OpRequest::Mods {
        instance: "survival".to_owned(),
        add: vec!["nope.nope".to_owned()],
        remove: vec![],
        update: vec![],
        build: Some(false),
    });
    assert_failed(&outcome);
    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    assert!(
        !card
            .manifest
            .mods
            .iter()
            .any(|(id, _)| id.as_str() == "nope.nope")
    );
    assert_eq!(card.packages.len(), 1, "the lock is unchanged too");
}

#[test]
fn update_picks_up_a_newer_package_version() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "dev",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));

    let before = catalog::load_instance(&world.paths, "dev").unwrap();
    assert_eq!(before.packages[0].version, "1.0.0");

    world.add_package("pwc.hotbar", "1.1.0", &[]);
    let (outcome, _sink) = world.run(&OpRequest::Update {
        instance: "dev".to_owned(),
        build: Some(false),
        reset_lock: false,
    });
    let summary = assert_ok(&outcome);
    assert!(summary.contains("mod"), "{summary}");

    let after = catalog::load_instance(&world.paths, "dev").unwrap();
    assert_eq!(after.packages[0].version, "1.1.0");

    // Already up to date the second time.
    let (outcome, _sink) = world.run(&OpRequest::Update {
        instance: "dev".to_owned(),
        build: Some(false),
        reset_lock: false,
    });
    let summary = assert_ok(&outcome);
    assert!(summary.contains("Already up to date"), "{summary}");
}

#[test]
#[cfg_attr(
    windows,
    ignore = "Windows Play needs `watt-start op game-guard` as the current executable; a watt-core \
              test binary is not watt-start"
)]
fn play_launches_detached_with_the_right_environment_and_refuses_while_running() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        true,
    ));

    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    let Readiness::Ready { game } = status::compute(&world.paths, &card).unwrap().readiness else {
        panic!("expected Ready after create(build: true)");
    };

    let (outcome, _sink) = world.run(&OpRequest::Play {
        instance: "survival".to_owned(),
        last_good: false,
        args: vec![],
    });
    assert_ok(&outcome);

    let env_file = std::path::PathBuf::from(format!("{}.env", game.display()));
    let deadline = Instant::now() + Duration::from_secs(5);
    let env_text = loop {
        if let Ok(text) = std::fs::read_to_string(&env_file) {
            break text;
        }
        if Instant::now() > deadline {
            panic!("the fake game never wrote {}", env_file.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(env_text.contains("PWC_INSTANCE=survival"), "{env_text}");
    assert!(env_text.contains("PWC_ENVIRONMENT=sha256:"), "{env_text}");
    assert!(env_text.contains("WATT_DATA_DIR="), "{env_text}");
    assert!(env_text.contains("WATT_ASSET_DIR="), "{env_text}");

    let game_toml = InstancePaths::new(&card.dir).running();
    assert!(game_toml.is_file());

    // `game.lock` (not `game.toml`) is the truth: held for as long as the game runs, and that is
    // what `status::compute` and a second `Play`/`Remove` go by.
    assert!(FileLock::is_held(
        &InstancePaths::new(&card.dir).game_lock()
    ));
    let running = status::compute(&world.paths, &card).unwrap().running;
    assert!(running.is_some(), "status should report the game running");

    // The game is still running (it sleeps for a few seconds): a second Play, and Remove, refuse.
    let (outcome, _sink) = world.run(&OpRequest::Play {
        instance: "survival".to_owned(),
        last_good: false,
        args: vec![],
    });
    let (error, _) = assert_failed(&outcome);
    assert!(error.unwrap().contains("is running"), "{error:?}");

    let (outcome, _sink) = world.run(&OpRequest::Remove {
        instance: "survival".to_owned(),
    });
    let (error, _) = assert_failed(&outcome);
    assert!(error.unwrap().contains("is running"), "{error:?}");
}

#[test]
fn rollback_restores_the_previous_generations_lock() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    world.add_package("pwc.inventory", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        true,
    ));

    let gen1 = catalog::load_instance(&world.paths, "survival")
        .unwrap()
        .meta
        .generations[0]
        .id;

    let (outcome, _sink) = world.run(&OpRequest::Mods {
        instance: "survival".to_owned(),
        add: vec!["pwc.inventory".to_owned()],
        remove: vec![],
        update: vec![],
        build: Some(true),
    });
    assert_ok(&outcome);
    let after_add = catalog::load_instance(&world.paths, "survival").unwrap();
    assert_eq!(after_add.packages.len(), 2);
    assert_ne!(after_add.meta.generations[0].id, gen1);

    let (outcome, _sink) = world.run(&OpRequest::Rollback {
        instance: "survival".to_owned(),
        generation: Some(gen1),
    });
    let summary = assert_ok(&outcome);
    assert!(summary.contains(&gen1.to_string()), "{summary}");

    let rolled_back = catalog::load_instance(&world.paths, "survival").unwrap();
    assert_eq!(rolled_back.packages.len(), 1);
    assert_eq!(rolled_back.packages[0].id, "pwc.hotbar");
    // The wanted mods come back too, so the next build keeps the rolled-back set.
    let wanted: Vec<String> = rolled_back
        .manifest
        .mods
        .keys()
        .map(ToString::to_string)
        .collect();
    assert_eq!(wanted, ["pwc.hotbar"]);
    let (outcome, _sink) = world.run(&OpRequest::Build {
        instance: "survival".to_owned(),
        force: false,
        profile: None,
    });
    assert_ok(&outcome);
    let rebuilt = catalog::load_instance(&world.paths, "survival").unwrap();
    assert_eq!(rebuilt.packages.len(), 1);

    // ...and rolling forward again restores the inventory.
    let (outcome, _sink) = world.run(&OpRequest::Rollback {
        instance: "survival".to_owned(),
        generation: Some(after_add.meta.generations[0].id),
    });
    assert_ok(&outcome);
    let forward = catalog::load_instance(&world.paths, "survival").unwrap();
    assert_eq!(forward.manifest.mods.len(), 2);
    assert_eq!(forward.packages.len(), 2);
}

#[test]
fn duplicate_and_remove_an_instance() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));

    let (outcome, _sink) = world.run(&OpRequest::Duplicate {
        instance: "survival".to_owned(),
        new_name: "survival-2".to_owned(),
        display_name: Some("Survival Two".to_owned()),
        with_worlds: false,
    });
    assert_ok(&outcome);
    let dup = catalog::load_instance(&world.paths, "survival-2").unwrap();
    assert_eq!(dup.packages.len(), 1);
    assert_eq!(dup.meta.title("survival-2"), "Survival Two");

    let (outcome, _sink) = world.run(&OpRequest::Remove {
        instance: "survival-2".to_owned(),
    });
    assert_ok(&outcome);
    assert!(catalog::load_instance(&world.paths, "survival-2").is_err());
    // The original is untouched.
    assert!(catalog::load_instance(&world.paths, "survival").is_ok());
}

#[test]
fn maintenance_gc_store_dry_run_keeps_generation_lock_packages() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));

    let (outcome, _sink) = world.run(&OpRequest::Maintenance {
        clean_cache: false,
        gc_snapshots: false,
        gc_store: true,
        dry_run: true,
    });
    let summary = assert_ok(&outcome);
    assert!(summary.contains("0 store"), "{summary}");
}

#[test]
fn a_busy_instance_refuses_another_op() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));

    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    let _held = FileLock::acquire(&InstancePaths::new(&card.dir).op_lock()).unwrap();

    let (outcome, _sink) = world.run(&OpRequest::Build {
        instance: "survival".to_owned(),
        force: false,
        profile: None,
    });
    let (error, recovery) = assert_failed(&outcome);
    assert!(error.unwrap().contains("another task"), "{error:?}");
    assert!(recovery.contains(&Recovery::Retry));
}

#[test]
fn a_failing_build_gives_force_rebuild_recovery() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    world.set_cargo_should_fail(true);

    let outcome = create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        true,
    );
    let (error, recovery) = assert_failed(&outcome);
    assert!(error.unwrap().contains("cargo"), "{error:?}");
    assert!(recovery.contains(&Recovery::Retry));
    assert!(recovery.contains(&Recovery::ForceRebuild));
}

#[test]
fn catalog_lists_instances_with_packages_and_game_info() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));

    let mut config = pwc_instance::Config::load(&world.paths.pwc.config_file()).unwrap();
    config.active_instance = Some("survival".to_owned());
    config.save(&world.paths.pwc.config_file()).unwrap();

    let cards = catalog::list_instances(&world.paths).unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].name, "survival");
    assert_eq!(cards[0].packages[0].name, "Test pwc.hotbar");
    assert_eq!(cards[0].game.version.as_deref(), Some("2.0.0"));
    assert_eq!(cards[0].game.api.as_deref(), Some("1.0.0"));
    assert!(cards[0].game.environment.is_some());
    assert!(cards[0].active);
}

#[test]
fn status_is_needs_build_after_a_mod_change() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    world.add_package("pwc.inventory", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        true,
    ));

    let (outcome, _sink) = world.run(&OpRequest::Mods {
        instance: "survival".to_owned(),
        add: vec!["pwc.inventory".to_owned()],
        remove: vec![],
        update: vec![],
        build: Some(false),
    });
    assert_ok(&outcome);

    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    let computed = status::compute(&world.paths, &card).unwrap();
    assert!(
        matches!(computed.readiness, Readiness::NeedsBuild { .. }),
        "{:?}",
        computed.readiness
    );
}

// --- Managed-source tests: local git checkouts stand in for the game/engine/mods mirrors. ---

fn managed(channel: &str) -> SourceSpec {
    SourceSpec::Managed {
        channel: channel.to_owned(),
        engine_ref: watt_core::meta::AUTO.to_owned(),
        mods_ref: watt_core::meta::AUTO.to_owned(),
    }
}

#[test]
fn create_managed_instance_downloads_and_locks() {
    let mut world = World::new();
    let remotes = world.add_managed_remotes();
    world.add_remote_package(&remotes.mods, "pwc.hotbar", "1.0.0", &[]);

    let outcome = create(&world, "main", managed("main"), &["pwc.hotbar"], true);
    let summary = assert_ok(&outcome);
    assert!(summary.contains("Created"), "{summary}");

    let card = catalog::load_instance(&world.paths, "main").unwrap();
    assert_eq!(card.packages.len(), 1);
    let watt_core::meta::Binding::Managed { pin: Some(pin), .. } = &card.meta.source else {
        panic!(
            "expected a pinned managed binding, got {:?}",
            card.meta.source
        );
    };
    assert_eq!(pin.game_ref, "main");
    assert!(
        matches!(
            status::compute(&world.paths, &card).unwrap().readiness,
            Readiness::Ready { .. }
        ),
        "{:?}",
        status::compute(&world.paths, &card).unwrap().readiness
    );
}

#[test]
fn update_managed_instance_moves_the_pin() {
    let mut world = World::new();
    let remotes = world.add_managed_remotes();
    assert_ok(&create(&world, "main", managed("main"), &[], false));

    let before = catalog::load_instance(&world.paths, "main").unwrap();
    let watt_core::meta::Binding::Managed {
        pin: Some(before_pin),
        ..
    } = before.meta.source
    else {
        panic!("expected a pinned managed binding");
    };

    // Move the game's `main` branch forward.
    std::fs::write(remotes.game.join("CHANGED"), "x").unwrap();
    support::commit_all(&remotes.game, "game: a change");

    let (outcome, _sink) = world.run(&OpRequest::Update {
        instance: "main".to_owned(),
        build: Some(false),
        reset_lock: false,
    });
    let summary = assert_ok(&outcome);
    assert!(summary.contains("Updated to main"), "{summary}");

    let after = catalog::load_instance(&world.paths, "main").unwrap();
    let watt_core::meta::Binding::Managed {
        pin: Some(after_pin),
        ..
    } = after.meta.source
    else {
        panic!("expected a pinned managed binding");
    };
    assert_ne!(before_pin.game, after_pin.game);

    // Already up to date the second time.
    let (outcome, _sink) = world.run(&OpRequest::Update {
        instance: "main".to_owned(),
        build: Some(false),
        reset_lock: false,
    });
    let summary = assert_ok(&outcome);
    assert!(summary.contains("Already up to date"), "{summary}");
}

#[test]
fn status_reports_needs_build_when_only_the_engine_changed() {
    let mut world = World::new();
    let remotes = world.add_managed_remotes();
    assert_ok(&create(&world, "main", managed("main"), &[], true));
    let before = catalog::load_instance(&world.paths, "main").unwrap();
    assert!(matches!(
        status::compute(&world.paths, &before).unwrap().readiness,
        Readiness::Ready { .. }
    ));

    // Move only the engine forward; the game and mods stay put, so the environment hash (which
    // never covers the engine) is unchanged.
    std::fs::write(remotes.engine.join("CHANGED"), "x").unwrap();
    support::commit_all(&remotes.engine, "engine: a change");
    let (outcome, _sink) = world.run(&OpRequest::Update {
        instance: "main".to_owned(),
        build: Some(false),
        reset_lock: false,
    });
    assert_ok(&outcome);

    let after = catalog::load_instance(&world.paths, "main").unwrap();
    match status::compute(&world.paths, &after).unwrap().readiness {
        Readiness::NeedsBuild { reason } => {
            assert!(reason.contains("engine"), "{reason}");
        }
        other => panic!("expected NeedsBuild, got {other:?}"),
    }
}

#[test]
fn set_source_leaves_the_old_binding_intact_on_a_nonexistent_channel() {
    let mut world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));
    let _remotes = world.add_managed_remotes();

    let (outcome, _sink) = world.run(&OpRequest::SetSource {
        instance: "survival".to_owned(),
        source: managed("no-such-branch"),
        build: Some(false),
    });
    assert_failed(&outcome);

    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    assert_eq!(card.meta.source, watt_core::meta::Binding::PwcDefault);
}

#[test]
fn duplicate_refuses_an_existing_destination_and_leaves_no_staging_dir() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "a",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));
    assert_ok(&create(
        &world,
        "b",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));

    let (outcome, _sink) = world.run(&OpRequest::Duplicate {
        instance: "a".to_owned(),
        new_name: "b".to_owned(),
        display_name: None,
        with_worlds: false,
    });
    let (error, _) = assert_failed(&outcome);
    assert!(error.unwrap().contains("already exists"), "{error:?}");

    let leftovers: Vec<_> = std::fs::read_dir(world.paths.pwc.instances())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-dup-"))
        .collect();
    assert!(leftovers.is_empty(), "staging directory left behind");
    // `b` itself is untouched.
    assert_eq!(
        catalog::load_instance(&world.paths, "b")
            .unwrap()
            .packages
            .len(),
        1
    );
}

#[test]
#[cfg_attr(
    windows,
    ignore = "Windows Play needs `watt-start op game-guard` as the current executable; a watt-core \
              test binary is not watt-start"
)]
fn duplicate_refuses_to_copy_worlds_while_the_source_is_running() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        true,
    ));
    assert_ok(
        &world
            .run(&OpRequest::Play {
                instance: "survival".to_owned(),
                last_good: false,
                args: vec![],
            })
            .0,
    );

    let (outcome, _sink) = world.run(&OpRequest::Duplicate {
        instance: "survival".to_owned(),
        new_name: "copy".to_owned(),
        display_name: None,
        with_worlds: true,
    });
    let (error, _) = assert_failed(&outcome);
    assert!(error.unwrap().contains("is running"), "{error:?}");
}

#[test]
fn maintenance_refuses_while_an_instance_is_busy() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));
    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    let _held = FileLock::acquire(&InstancePaths::new(&card.dir).op_lock()).unwrap();

    let (outcome, _sink) = world.run(&OpRequest::Maintenance {
        clean_cache: false,
        gc_snapshots: false,
        gc_store: true,
        dry_run: true,
    });
    let (error, _) = assert_failed(&outcome);
    assert!(error.unwrap().contains("is busy"), "{error:?}");
}

#[test]
fn rollback_relocks_when_a_generations_package_is_missing_from_the_store() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        true,
    ));
    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    let gen1 = card.meta.generations[0].id;
    let gen1_lock =
        pwc_manifest::Lockfile::from_file(&InstancePaths::new(&card.dir).generation_lock(gen1))
            .unwrap();
    let hash = gen1_lock.packages[0].hash.clone();

    world.add_package("pwc.inventory", "1.0.0", &[]);
    assert_ok(
        &world
            .run(&OpRequest::Mods {
                instance: "survival".to_owned(),
                add: vec!["pwc.inventory".to_owned()],
                remove: vec!["pwc.hotbar".to_owned()],
                update: vec![],
                build: Some(true),
            })
            .0,
    );

    // Simulate the package store losing generation 1's package externally (e.g. a concurrent
    // `pwc store gc` that did not know about this generation).
    let store = pwc_store::Store::open(world.paths.pwc.store()).unwrap();
    assert!(store.remove(&hash).unwrap());

    let (outcome, _sink) = world.run(&OpRequest::Rollback {
        instance: "survival".to_owned(),
        generation: Some(gen1),
    });
    let summary = assert_ok(&outcome);
    assert!(summary.contains(&gen1.to_string()), "{summary}");

    let rolled_back = catalog::load_instance(&world.paths, "survival").unwrap();
    assert_eq!(rolled_back.packages.len(), 1);
    assert_eq!(rolled_back.packages[0].id, "pwc.hotbar");
}

#[test]
fn build_fails_when_pwc_lock_changed_underneath_it() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar"],
        false,
    ));

    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    let instance = pwc_instance::Instance::open(
        &world.paths.pwc,
        &pwc_manifest::InstanceName::parse("survival").unwrap(),
    )
    .unwrap();
    let resolved =
        watt_core::binding::resolve(&world.paths, &world.settings, &card.meta.source).unwrap();
    // The lock as it stood right after locking, kept around like a caller that locked once and
    // is about to build — meanwhile another tool relocks the instance (simulated below).
    let stale_lock = instance.lockfile().unwrap().unwrap();

    world.add_package("pwc.inventory", "1.0.0", &[]);
    assert_ok(
        &world
            .run(&OpRequest::Mods {
                instance: "survival".to_owned(),
                add: vec!["pwc.inventory".to_owned()],
                remove: vec![],
                update: vec![],
                build: Some(false),
            })
            .0,
    );

    let sink = std::sync::Arc::new(watt_core::events::VecSink::default());
    let reporter =
        watt_core::events::Reporter::new(sink, "t", watt_core::events::CancelToken::default());
    let err = watt_core::build::build(
        &world.paths,
        &world.settings,
        &instance,
        &resolved,
        &stale_lock,
        None,
        false,
        &reporter,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("pwc.lock changed while building"),
        "{err:#}"
    );
}

/// `Mods.update` is the set `LockOptions` prefers newest for. A sibling package stays pinned.
#[test]
fn mods_update_bumps_only_the_named_package() {
    let world = World::new();
    world.add_package("pwc.hotbar", "1.0.0", &[]);
    world.add_package("pwc.inventory", "1.0.0", &[]);
    assert_ok(&create(
        &world,
        "survival",
        SourceSpec::PwcDefault,
        &["pwc.hotbar", "pwc.inventory"],
        false,
    ));
    world.add_package("pwc.hotbar", "1.1.0", &[]);
    world.add_package("pwc.inventory", "1.1.0", &[]);

    let (outcome, _sink) = world.run(&OpRequest::Mods {
        instance: "survival".to_owned(),
        add: vec![],
        remove: vec![],
        update: vec!["pwc.hotbar".to_owned()],
        build: Some(false),
    });
    assert_ok(&outcome);
    let card = catalog::load_instance(&world.paths, "survival").unwrap();
    let version = |id: &str| {
        card.packages
            .iter()
            .find(|pkg| pkg.id == id)
            .map(|pkg| pkg.version.as_str())
            .unwrap_or("missing")
            .to_owned()
    };
    assert_eq!(version("pwc.hotbar"), "1.1.0");
    assert_eq!(version("pwc.inventory"), "1.0.0");
}
