//! The demo backend: realistic fake data and simulated ops, for screenshots and `WATT_START_DEMO=1`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use pwc_manifest::{
    InstanceManifest, InstanceName, ModRequirement, ModSource, PackageId, VersionReq,
};
use watt_core::catalog::{AvailableMod, GameInfo, InstanceCard, PackageCard};
use watt_core::doctor::{Check, Level};
use watt_core::events::{
    Change, Commit, Envelope, Event, LogStream, OpKind, OpOutcome, PROTOCOL, Phase, PhaseOutcome,
    Recovery, RepoLog,
};
use watt_core::git::BranchInfo;
use watt_core::meta::{Binding, BuiltGame, Generation, LastOp, Meta, Pin};
use watt_core::ops::OpRequest;
use watt_core::paths::Repo;
use watt_core::settings::Settings;
use watt_core::status::{InstanceStatus, Readiness};
use watt_core::view::OpView;

use super::{Backend, InstanceEntry, Loaded, MetaEdit, RunningOp};

/// Deterministic 40-character fake hash from any seed string.
fn fake_sha(seed: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    let hex = format!("{h:016x}");
    hex.repeat(3)[..40].to_owned()
}

/// A fixture constructor with one argument per `PackageCard` field worth varying; a builder
/// would be more ceremony than the handful of call sites below need.
#[allow(clippy::too_many_arguments)]
fn pkg(
    id: &str,
    name: &str,
    version: &str,
    kind: &str,
    desc: &str,
    direct: bool,
    categories: &[&str],
    authors: &[&str],
    deps: &[&str],
    bundles: &[&str],
) -> PackageCard {
    PackageCard {
        id: id.to_owned(),
        name: name.to_owned(),
        version: version.to_owned(),
        kind: kind.to_owned(),
        description: desc.to_owned(),
        direct,
        categories: categories.iter().map(|s| s.to_string()).collect(),
        authors: authors.iter().map(|s| s.to_string()).collect(),
        license: "MIT".to_owned(),
        dependencies: deps.iter().map(|s| s.to_string()).collect(),
        in_bundles: bundles.iter().map(|s| s.to_string()).collect(),
        source: "repository:pwc-package-manager".to_owned(),
    }
}

/// Same rationale as [`pkg`]: a plain fixture constructor, not public API.
#[allow(clippy::too_many_arguments)]
fn available(
    id: &str,
    name: &str,
    version: &str,
    kind: &str,
    desc: &str,
    categories: &[&str],
    deps: &[&str],
    members: &[&str],
) -> AvailableMod {
    AvailableMod {
        id: id.to_owned(),
        name: name.to_owned(),
        version: version.to_owned(),
        versions: vec![version.to_owned()],
        kind: kind.to_owned(),
        description: desc.to_owned(),
        categories: categories.iter().map(|s| s.to_string()).collect(),
        keywords: Vec::new(),
        authors: vec!["Project Watt Cubed contributors".to_owned()],
        license: "MIT".to_owned(),
        dependencies: deps.iter().map(|s| s.to_string()).collect(),
        members: members.iter().map(|s| s.to_string()).collect(),
        pwc_api: (kind != "bundle").then(|| ">=0.4".to_owned()),
    }
}

/// Mods requested in the manifest but absent from the lock. Only the modded fixture has one,
/// so every other instance keeps an empty manifest.
fn unresolved_ids(name: &str) -> &'static [&'static str] {
    match name {
        "modded" => &["pwc.phantom-relay"],
        _ => &[],
    }
}

fn manifest_for(name: &str, mods: &[PackageCard]) -> InstanceManifest {
    let mut manifest = InstanceManifest::new(InstanceName::parse(name).unwrap(), VersionReq::STAR);
    let missing = unresolved_ids(name);
    if missing.is_empty() {
        return manifest;
    }
    for pkg in mods.iter().filter(|pkg| pkg.direct) {
        manifest.mods.insert(
            PackageId::parse(&pkg.id).expect("fixture id"),
            ModRequirement {
                version: VersionReq::STAR,
                source: ModSource::Registry,
            },
        );
    }
    for id in missing {
        manifest.mods.insert(
            PackageId::parse(id).expect("fixture id"),
            ModRequirement {
                version: VersionReq::STAR,
                source: ModSource::Registry,
            },
        );
    }
    manifest
}

fn pin(game_ref: &str, seed: &str) -> Pin {
    Pin {
        game: fake_sha(&format!("{seed}-game")),
        engine: fake_sha(&format!("{seed}-engine")),
        mods: fake_sha(&format!("{seed}-mods")),
        game_ref: game_ref.to_owned(),
        engine_ref: game_ref.to_owned(),
        mods_ref: game_ref.to_owned(),
    }
}

fn generation(id: u64, created: u64, version: &str, seed: &str, built: bool) -> Generation {
    Generation {
        id,
        created,
        source: Binding::Managed {
            channel: "main".into(),
            engine_ref: watt_core::meta::AUTO.into(),
            mods_ref: watt_core::meta::AUTO.into(),
            pin: Some(pin("main", seed)),
        },
        pwc_source: PathBuf::from("/var/lib/pwc/sources/v1/pwc-demo"),
        environment: format!("sha256:{}", fake_sha(seed)),
        game_version: version.to_owned(),
        packages: vec!["pwc.essentials 1.4.0".into(), "pwc.hotbar 2.1.0".into()],
        build: built.then(|| BuiltGame {
            build_id: fake_sha(&format!("{seed}-build"))[..12].to_owned(),
            game: PathBuf::from(format!("/var/lib/pwc/cache/{seed}/game")),
            profile: "release".to_owned(),
            wrapper: Vec::new(),
            duration_ms: 11 * 60_000 + 24_000,
            finished: created,
            rustc: "rustc 1.95.0 (abcdef123 2026-10-01)".to_owned(),
        }),
    }
}

/// One instance's fixture, built from a few knobs.
struct Fixture {
    name: &'static str,
    display: Option<&'static str>,
    group: Option<&'static str>,
    favorite: bool,
    active: bool,
    binding: Binding,
    readiness: Readiness,
    running: Option<u32>,
    mods: Vec<PackageCard>,
    updates: Option<Vec<RepoLog>>,
}

fn fixtures() -> Vec<Fixture> {
    let essentials = vec![
        pkg(
            "pwc.essentials",
            "Essentials",
            "1.4.0",
            "bundle",
            "The recommended starter set.",
            true,
            &["bundle"],
            &["Project Watt Cubed contributors"],
            &["pwc.hotbar", "pwc.waypoints"],
            &[],
        ),
        pkg(
            "pwc.hotbar",
            "Quick Hotbar",
            "2.1.0",
            "mod",
            "A faster, configurable hotbar.",
            false,
            &["interface"],
            &["gusahlg"],
            &["pwc.ui-kit"],
            &["pwc.essentials"],
        ),
        pkg(
            "pwc.waypoints",
            "Waypoints",
            "1.0.3",
            "mod",
            "Place and share waypoints.",
            false,
            &["gameplay", "interface"],
            &["gusahlg"],
            &[],
            &["pwc.essentials"],
        ),
        pkg(
            "pwc.ui-kit",
            "UI Kit",
            "0.9.1",
            "library",
            "Shared interface widgets.",
            false,
            &["library"],
            &["gusahlg"],
            &[],
            &[],
        ),
    ];
    let mut dev_toolkit = essentials.clone();
    dev_toolkit.push(pkg(
        "pwc.dev-toolkit",
        "Developer Toolkit",
        "0.3.0",
        "mod",
        "Debug overlays, chunk borders, teleport.",
        true,
        &["tools"],
        &["gusahlg"],
        &[],
        &[],
    ));

    vec![
        Fixture {
            name: "survival",
            display: Some("Survival"),
            group: None,
            favorite: true,
            active: true,
            binding: Binding::Managed {
                channel: "main".into(),
                engine_ref: watt_core::meta::AUTO.into(),
                mods_ref: watt_core::meta::AUTO.into(),
                pin: Some(pin("main", "survival")),
            },
            readiness: Readiness::Ready {
                game: PathBuf::from("/var/lib/pwc/cache/survival/game"),
            },
            running: None,
            mods: dev_toolkit.clone(),
            updates: None,
        },
        Fixture {
            name: "frontier",
            display: Some("Frontier"),
            group: None,
            favorite: false,
            active: false,
            binding: Binding::Managed {
                channel: "experimental".into(),
                engine_ref: watt_core::meta::AUTO.into(),
                mods_ref: watt_core::meta::AUTO.into(),
                pin: Some(pin("experimental", "frontier")),
            },
            readiness: Readiness::Ready {
                game: PathBuf::from("/var/lib/pwc/cache/frontier/game"),
            },
            running: None,
            mods: essentials.clone(),
            updates: Some(vec![
                RepoLog {
                    repo: Repo::Game,
                    commits: vec![
                        Commit {
                            sha: fake_sha("frontier-game-1"),
                            summary: "Zeldovich law: tune drive coefficients".into(),
                            author: "gusahlg".into(),
                            time: 1_760_000_000,
                        },
                        Commit {
                            sha: fake_sha("frontier-game-2"),
                            summary: "Fix cruise freeze on world switch".into(),
                            author: "gusahlg".into(),
                            time: 1_759_990_000,
                        },
                    ],
                    truncated: false,
                },
                RepoLog {
                    repo: Repo::Engine,
                    commits: vec![Commit {
                        sha: fake_sha("frontier-engine-1"),
                        summary: "Streaming: EpochHeap for async relight".into(),
                        author: "gusahlg".into(),
                        time: 1_759_980_000,
                    }],
                    truncated: false,
                },
            ]),
        },
        Fixture {
            name: "dev-checkout",
            display: Some("Dev checkout"),
            group: Some("Experiments"),
            favorite: false,
            active: false,
            binding: Binding::Linked {
                pwc_source: PathBuf::from("/home/player/repos/project_watt_cubed"),
                repositories: Vec::new(),
            },
            readiness: Readiness::Ready {
                game: PathBuf::from("/var/lib/pwc/cache/dev-checkout/game"),
            },
            running: None,
            mods: essentials.clone(),
            updates: None,
        },
        Fixture {
            name: "pwc-cli",
            display: None,
            group: None,
            favorite: false,
            active: false,
            binding: Binding::PwcDefault,
            readiness: Readiness::Ready {
                game: PathBuf::from("/var/lib/pwc/cache/pwc-cli/game"),
            },
            running: None,
            mods: essentials.clone(),
            updates: None,
        },
        Fixture {
            name: "survival-classic",
            display: Some("Survival Classic"),
            group: Some("Experiments"),
            favorite: false,
            active: false,
            binding: Binding::Managed {
                channel: "main".into(),
                engine_ref: watt_core::meta::AUTO.into(),
                mods_ref: watt_core::meta::AUTO.into(),
                pin: Some(pin("main", "classic")),
            },
            readiness: Readiness::NeedsBuild {
                reason: "3 mods changed".into(),
            },
            running: None,
            mods: essentials.clone(),
            updates: None,
        },
        Fixture {
            name: "flaky-build",
            display: Some("Flaky Build"),
            group: Some("Experiments"),
            favorite: false,
            active: false,
            binding: Binding::Managed {
                channel: "mp-fix".into(),
                engine_ref: watt_core::meta::AUTO.into(),
                mods_ref: watt_core::meta::AUTO.into(),
                pin: Some(pin("mp-fix", "flaky")),
            },
            readiness: Readiness::Broken {
                reason: "pwc.hotbar requires pwc-mod-api ^0.5, found 0.4.2".into(),
            },
            running: None,
            mods: essentials.clone(),
            updates: None,
        },
        Fixture {
            name: "modded",
            display: Some("Modded"),
            group: Some("Experiments"),
            favorite: false,
            active: false,
            binding: Binding::Managed {
                channel: "main".into(),
                engine_ref: watt_core::meta::AUTO.into(),
                mods_ref: watt_core::meta::AUTO.into(),
                pin: Some(pin("main", "modded")),
            },
            readiness: Readiness::Ready {
                game: PathBuf::from("/var/lib/pwc/cache/modded/game"),
            },
            running: None,
            mods: modded_packages(),
            updates: None,
        },
    ]
}

/// The modded instance: two bundles, their members, a handful of dependencies (including
/// libraries) and one requirement the lock does not have.
fn modded_packages() -> Vec<PackageCard> {
    const ROWS: &[InstalledSpec] = &[
        InstalledSpec::new(
            "pwc.essentials",
            "Essentials",
            "1.4.0",
            "bundle",
            "The recommended starter set.",
            true,
            &["bundle"],
            &["Project Watt Cubed contributors"],
            &["pwc.hotbar", "pwc.waypoints"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.expedition",
            "Expedition",
            "0.6.0",
            "bundle",
            "Maps, notes and the gear for a long trip.",
            true,
            &["bundle", "gameplay"],
            &["field-lab"],
            &["pwc.trail-map", "pwc.field-notes"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.ambient-audio",
            "Ambient Audio",
            "1.0.0",
            "mod",
            "Biome-aware ambient soundscapes.",
            true,
            &["audio"],
            &["lumen"],
            &["pwc.audio-core"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.backpacks",
            "Backpacks",
            "1.2.0",
            "mod",
            "Carry a second inventory on your back.",
            true,
            &["gameplay"],
            &["gusahlg"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.better-caves",
            "Better Caves",
            "1.2.0",
            "mod",
            "Richer cave generation with new biomes.",
            true,
            &["worldgen"],
            &["gusahlg"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.better-lighting",
            "Better Lighting",
            "1.1.2",
            "mod",
            "Soft sun, tinted torches and light shafts.",
            true,
            &["visuals"],
            &["lumen"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.boats",
            "Boats",
            "0.4.0",
            "mod",
            "Paddle the rivers and the open water.",
            true,
            &["gameplay"],
            &["gusahlg"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.crop-boost",
            "Crop Boost",
            "0.3.1",
            "mod",
            "Faster growth when the soil is watered.",
            true,
            &["farming"],
            &["field-lab"],
            &["pwc.farm-lib"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.deep-ores",
            "Deep Ores",
            "0.4.2",
            "mod",
            "Richer deposits below the deepslate line.",
            true,
            &["worldgen"],
            &["ore-guild"],
            &["pwc.geology"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.farming-plus",
            "Farming Expansion",
            "0.8.0",
            "mod",
            "New crops, irrigation and a seasonal calendar.",
            true,
            &["farming"],
            &["field-lab"],
            &["pwc.farm-lib"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.inventory-sort",
            "Inventory Sorting",
            "0.9.0",
            "mod",
            "Stack, sort and pin slots in every container.",
            true,
            &["gameplay"],
            &["gusahlg"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.lanterns",
            "Lanterns",
            "0.9.0",
            "mod",
            "Placeable lanterns with a warm falloff.",
            true,
            &["visuals"],
            &["lumen"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.minimap",
            "Minimap",
            "1.3.0",
            "mod",
            "A corner map of the chunks around you.",
            true,
            &["interface"],
            &["mina"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.more-ores",
            "More Ores",
            "1.0.0",
            "mod",
            "Copper, tin and the alloys the base game left out.",
            true,
            &["worldgen"],
            &["ore-guild"],
            &["pwc.geology"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.night-vision",
            "Night Vision",
            "0.5.0",
            "mod",
            "See in the dark without a torch in hand.",
            true,
            &["visuals"],
            &["lumen"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.ore-veins",
            "Ore Veins",
            "0.7.0",
            "mod",
            "Metals cluster in veins instead of single cubes.",
            true,
            &["worldgen"],
            &["ore-guild"],
            &["pwc.geology"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.shader-pack",
            "Shader Pack",
            "0.8.4",
            "mod",
            "Volumetric lighting and soft shadows.",
            true,
            &["visuals"],
            &["lumen"],
            &["pwc.ui-kit"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.signs-plus",
            "Signs Plus",
            "1.0.0",
            "mod",
            "Tinted signs with extra lines of text.",
            true,
            &["gameplay"],
            &["mina"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.soundscape",
            "Field Recordings",
            "1.0.1",
            "mod",
            "Footsteps, weather and caves captured on location.",
            true,
            &["audio"],
            &["lumen"],
            &["pwc.audio-core"],
            &[],
        ),
        InstalledSpec::new(
            "pwc.weather-plus",
            "Weather Plus",
            "0.5.2",
            "mod",
            "Storms, fog banks and seasonal cycles.",
            true,
            &["worldgen", "visuals"],
            &["field-lab"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.hotbar",
            "Quick Hotbar",
            "2.1.0",
            "mod",
            "A faster, configurable hotbar.",
            false,
            &["interface"],
            &["gusahlg"],
            &["pwc.ui-kit"],
            &["pwc.essentials"],
        ),
        InstalledSpec::new(
            "pwc.waypoints",
            "Waypoints",
            "1.0.3",
            "mod",
            "Place and share waypoints.",
            false,
            &["gameplay", "interface"],
            &["gusahlg"],
            &[],
            &["pwc.essentials"],
        ),
        InstalledSpec::new(
            "pwc.trail-map",
            "Trail Map",
            "0.6.0",
            "mod",
            "Breadcrumbs and named pins along a route.",
            false,
            &["interface"],
            &["field-lab"],
            &[],
            &["pwc.expedition"],
        ),
        InstalledSpec::new(
            "pwc.field-notes",
            "Field Notes",
            "0.6.0",
            "mod",
            "A journal tied to the spot you wrote it.",
            false,
            &["gameplay"],
            &["field-lab"],
            &[],
            &["pwc.expedition"],
        ),
        InstalledSpec::new(
            "pwc.geology",
            "Geology",
            "1.1.0",
            "mod",
            "Shared strata tables the ore mods build on.",
            false,
            &["worldgen"],
            &["ore-guild"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.ui-kit",
            "UI Kit",
            "0.9.1",
            "library",
            "Shared interface widgets.",
            false,
            &["library"],
            &["gusahlg"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.audio-core",
            "Audio Core",
            "0.2.0",
            "library",
            "Sample mixer the sound packs share.",
            false,
            &["library"],
            &["lumen"],
            &[],
            &[],
        ),
        InstalledSpec::new(
            "pwc.farm-lib",
            "Farm Lib",
            "0.3.0",
            "library",
            "Soil, moisture and crop stages for farm mods.",
            false,
            &["library"],
            &["field-lab"],
            &[],
            &[],
        ),
    ];
    ROWS.iter().copied().map(InstalledSpec::package).collect()
}

#[derive(Clone, Copy)]
struct InstalledSpec {
    id: &'static str,
    name: &'static str,
    version: &'static str,
    kind: &'static str,
    desc: &'static str,
    direct: bool,
    categories: &'static [&'static str],
    authors: &'static [&'static str],
    deps: &'static [&'static str],
    bundles: &'static [&'static str],
}

impl InstalledSpec {
    #[allow(clippy::too_many_arguments)]
    const fn new(
        id: &'static str,
        name: &'static str,
        version: &'static str,
        kind: &'static str,
        desc: &'static str,
        direct: bool,
        categories: &'static [&'static str],
        authors: &'static [&'static str],
        deps: &'static [&'static str],
        bundles: &'static [&'static str],
    ) -> Self {
        Self {
            id,
            name,
            version,
            kind,
            desc,
            direct,
            categories,
            authors,
            deps,
            bundles,
        }
    }

    fn package(self) -> PackageCard {
        pkg(
            self.id,
            self.name,
            self.version,
            self.kind,
            self.desc,
            self.direct,
            self.categories,
            self.authors,
            self.deps,
            self.bundles,
        )
    }
}

fn build_entry(f: Fixture, now: u64) -> InstanceEntry {
    let mut meta = Meta {
        format: 1,
        display_name: f.display.map(str::to_owned),
        color: None,
        group: f.group.map(str::to_owned),
        favorite: f.favorite,
        notes: String::new(),
        created: Some(now - 30 * 86_400),
        last_played: Some(now - 2 * 3600),
        source: f.binding,
        generations: vec![
            generation(3, now - 3600, "0.6.1", &format!("{}-3", f.name), true),
            generation(2, now - 86_400, "0.6.0", &format!("{}-2", f.name), true),
            generation(1, now - 7 * 86_400, "0.5.3", &format!("{}-1", f.name), true),
        ],
        last_op: None,
    };
    if matches!(f.readiness, Readiness::Broken { .. }) {
        meta.last_op = Some(LastOp {
            op: OpKind::Build,
            finished: now - 1800,
            ok: false,
            error: Some("resolving mods: pwc.hotbar requires pwc-mod-api ^0.5".into()),
            log: Some(PathBuf::from("/var/lib/pwc/watt-start/logs/demo.log")),
        });
    }

    let branch = match &meta.source {
        Binding::Managed { channel, .. } => Some(channel.clone()),
        Binding::Linked { .. } | Binding::PwcDefault => Some("main".to_owned()),
    };
    let pin = match &meta.source {
        Binding::Managed { pin, .. } => pin.clone(),
        _ => None,
    };

    let card = InstanceCard {
        name: f.name.to_owned(),
        dir: PathBuf::from(format!("/var/lib/pwc/instances/{}", f.name)),
        manifest: manifest_for(f.name, &f.mods),
        meta: meta.clone(),
        lock: None,
        packages: f.mods,
        game: GameInfo {
            version: Some("0.6.1".into()),
            api: Some("0.4.2".into()),
            revision: pin.as_ref().map(|p| p.game.clone()),
            dirty: false,
            source_dir: Some(PathBuf::from(format!("/var/lib/pwc/sources/v1/{}", f.name))),
            branch,
            engine_revision: pin.as_ref().map(|p| p.engine.clone()),
            mods_revision: pin.as_ref().map(|p| p.mods.clone()),
            environment: Some(format!("sha256:{}", fake_sha(f.name))),
        },
        active: f.active,
        problems: Vec::new(),
    };
    let last_good = meta.last_good().cloned();
    let status = InstanceStatus {
        readiness: f.readiness,
        busy: false,
        running: f.running,
        last_good,
    };
    InstanceEntry { card, status }
}

/// Catalogue rows added for the modded instance. Authors are set here so [`available`] keeps
/// its default byline for the original entries.
#[allow(clippy::too_many_arguments)]
fn authored(
    id: &str,
    name: &str,
    version: &str,
    kind: &str,
    desc: &str,
    categories: &[&str],
    author: &str,
    deps: &[&str],
    members: &[&str],
) -> AvailableMod {
    let mut entry = available(id, name, version, kind, desc, categories, deps, members);
    entry.authors = vec![author.to_owned()];
    entry
}

fn extra_catalog() -> Vec<AvailableMod> {
    vec![
        authored(
            "pwc.minimap",
            "Minimap",
            "1.3.0",
            "mod",
            "A corner map of the chunks around you.",
            &["interface"],
            "mina",
            &[],
            &[],
        ),
        authored(
            "pwc.inventory-sort",
            "Inventory Sorting",
            "0.9.0",
            "mod",
            "Stack, sort and pin slots in every container.",
            &["gameplay"],
            "gusahlg",
            &[],
            &[],
        ),
        authored(
            "pwc.better-lighting",
            "Better Lighting",
            "1.1.2",
            "mod",
            "Soft sun, tinted torches and light shafts.",
            &["visuals"],
            "lumen",
            &[],
            &[],
        ),
        authored(
            "pwc.farming-plus",
            "Farming Expansion",
            "0.8.0",
            "mod",
            "New crops, irrigation and a seasonal calendar.",
            &["farming"],
            "field-lab",
            &["pwc.farm-lib"],
            &[],
        ),
        authored(
            "pwc.more-ores",
            "More Ores",
            "1.0.0",
            "mod",
            "Copper, tin and the alloys the base game left out.",
            &["worldgen"],
            "ore-guild",
            &["pwc.geology"],
            &[],
        ),
        authored(
            "pwc.soundscape",
            "Field Recordings",
            "1.0.1",
            "mod",
            "Footsteps, weather and caves captured on location.",
            &["audio"],
            "lumen",
            &["pwc.audio-core"],
            &[],
        ),
        authored(
            "pwc.ore-veins",
            "Ore Veins",
            "0.7.0",
            "mod",
            "Metals cluster in veins instead of single cubes.",
            &["worldgen"],
            "ore-guild",
            &["pwc.geology"],
            &[],
        ),
        authored(
            "pwc.deep-ores",
            "Deep Ores",
            "0.4.2",
            "mod",
            "Richer deposits below the deepslate line.",
            &["worldgen"],
            "ore-guild",
            &["pwc.geology"],
            &[],
        ),
        authored(
            "pwc.geology",
            "Geology",
            "1.1.0",
            "mod",
            "Shared strata tables the ore mods build on.",
            &["worldgen"],
            "ore-guild",
            &[],
            &[],
        ),
        authored(
            "pwc.expedition",
            "Expedition",
            "0.6.0",
            "bundle",
            "Maps, notes and the gear for a long trip.",
            &["bundle", "gameplay"],
            "field-lab",
            &["pwc.trail-map", "pwc.field-notes"],
            &["pwc.trail-map", "pwc.field-notes"],
        ),
        authored(
            "pwc.trail-map",
            "Trail Map",
            "0.6.0",
            "mod",
            "Breadcrumbs and named pins along a route.",
            &["interface"],
            "field-lab",
            &[],
            &[],
        ),
        authored(
            "pwc.field-notes",
            "Field Notes",
            "0.6.0",
            "mod",
            "A journal tied to the spot you wrote it.",
            &["gameplay"],
            "field-lab",
            &[],
            &[],
        ),
        authored(
            "pwc.backpacks",
            "Backpacks",
            "1.2.0",
            "mod",
            "Carry a second inventory on your back.",
            &["gameplay"],
            "gusahlg",
            &[],
            &[],
        ),
        authored(
            "pwc.night-vision",
            "Night Vision",
            "0.5.0",
            "mod",
            "See in the dark without a torch in hand.",
            &["visuals"],
            "lumen",
            &[],
            &[],
        ),
        authored(
            "pwc.crop-boost",
            "Crop Boost",
            "0.3.1",
            "mod",
            "Faster growth when the soil is watered.",
            &["farming"],
            "field-lab",
            &["pwc.farm-lib"],
            &[],
        ),
        authored(
            "pwc.signs-plus",
            "Signs Plus",
            "1.0.0",
            "mod",
            "Tinted signs with extra lines of text.",
            &["gameplay"],
            "mina",
            &[],
            &[],
        ),
        authored(
            "pwc.lanterns",
            "Lanterns",
            "0.9.0",
            "mod",
            "Placeable lanterns with a warm falloff.",
            &["visuals"],
            "lumen",
            &[],
            &[],
        ),
        authored(
            "pwc.boats",
            "Boats",
            "0.4.0",
            "mod",
            "Paddle the rivers and the open water.",
            &["gameplay"],
            "gusahlg",
            &[],
            &[],
        ),
        authored(
            "pwc.audio-core",
            "Audio Core",
            "0.2.0",
            "library",
            "Sample mixer the sound packs share.",
            &["library"],
            "lumen",
            &[],
            &[],
        ),
        authored(
            "pwc.farm-lib",
            "Farm Lib",
            "0.3.0",
            "library",
            "Soil, moisture and crop stages for farm mods.",
            &["library"],
            "field-lab",
            &[],
            &[],
        ),
        authored(
            "pwc.recipe-book",
            "Recipe Book",
            "1.0.0",
            "mod",
            "Every alloy and cooking recipe in one book.",
            &["gameplay"],
            "mina",
            &[],
            &[],
        ),
        authored(
            "pwc.clouds",
            "Better Clouds",
            "0.6.0",
            "mod",
            "Layered clouds that drift with the wind.",
            &["visuals"],
            "lumen",
            &[],
            &[],
        ),
        authored(
            "pwc.compass",
            "Compass",
            "1.0.0",
            "mod",
            "A heading marker that finds your last pin.",
            &["interface"],
            "mina",
            &[],
            &[],
        ),
        authored(
            "pwc.rivers",
            "Rivers",
            "0.5.0",
            "mod",
            "Carves riverbeds and fills them from rainfall.",
            &["worldgen"],
            "field-lab",
            &[],
            &[],
        ),
        authored(
            "pwc.builder-kit",
            "Builder Kit",
            "0.2.0",
            "bundle",
            "Chisel and scaffolding for big builds.",
            &["bundle", "tools"],
            "kiln",
            &["pwc.chisel", "pwc.scaffolding"],
            &["pwc.chisel", "pwc.scaffolding"],
        ),
        authored(
            "pwc.chisel",
            "Chisel",
            "0.2.0",
            "mod",
            "Cut slopes and detail into solid cubes.",
            &["tools"],
            "kiln",
            &[],
            &[],
        ),
        authored(
            "pwc.scaffolding",
            "Scaffolding",
            "0.2.0",
            "mod",
            "Temporary frames that hold you up.",
            &["tools"],
            "kiln",
            &[],
            &[],
        ),
    ]
}

/// The demo backend.
pub struct DemoBackend {
    settings: Settings,
    instances: Vec<InstanceEntry>,
    updates: BTreeMap<String, Vec<RepoLog>>,
    mods_catalog: Vec<AvailableMod>,
    doctor: Vec<Check>,
    next_op_id: u64,
    /// When non-zero, [`DemoBackend::start_op`] plays a running build with this many log lines
    /// already due. Only [`DemoBackend::stress`] sets it.
    stress_log_lines: usize,
}

impl DemoBackend {
    /// Build the fixture.
    pub fn new() -> Self {
        let now = watt_core::paths::now_secs();
        let mut updates = BTreeMap::new();
        let mut instances = Vec::new();
        for f in fixtures() {
            let name = f.name.to_owned();
            if let Some(u) = f.updates.clone() {
                updates.insert(name, u);
            }
            instances.push(build_entry(f, now));
        }

        let mut mods_catalog = vec![
            available(
                "pwc.essentials",
                "Essentials",
                "1.4.0",
                "bundle",
                "The recommended starter set.",
                &["bundle"],
                &["pwc.hotbar", "pwc.waypoints"],
                &["pwc.hotbar", "pwc.waypoints"],
            ),
            available(
                "pwc.dev-toolkit",
                "Developer Toolkit",
                "0.3.0",
                "mod",
                "Debug overlays, chunk borders, teleport.",
                &["tools"],
                &[],
                &[],
            ),
            available(
                "pwc.better-caves",
                "Better Caves",
                "1.2.0",
                "mod",
                "Richer cave generation with new biomes.",
                &["worldgen"],
                &[],
                &[],
            ),
            available(
                "pwc.shader-pack",
                "Shader Pack",
                "0.8.4",
                "mod",
                "Volumetric lighting and soft shadows.",
                &["visuals"],
                &["pwc.ui-kit"],
                &[],
            ),
            available(
                "pwc.ambient-audio",
                "Ambient Audio",
                "1.0.0",
                "mod",
                "Biome-aware ambient soundscapes.",
                &["audio"],
                &[],
                &[],
            ),
            available(
                "pwc.waypoints",
                "Waypoints",
                "1.0.3",
                "mod",
                "Place and share waypoints.",
                &["gameplay", "interface"],
                &[],
                &[],
            ),
            available(
                "pwc.hotbar",
                "Quick Hotbar",
                "2.1.0",
                "mod",
                "A faster, configurable hotbar.",
                &["interface"],
                &["pwc.ui-kit"],
                &[],
            ),
            available(
                "pwc.ui-kit",
                "UI Kit",
                "0.9.1",
                "library",
                "Shared interface widgets.",
                &["library"],
                &[],
                &[],
            ),
            available(
                "pwc.material-names",
                "Material Names",
                "0.4.0",
                "library",
                "Readable names for every element and alloy.",
                &["library"],
                &[],
                &[],
            ),
            available(
                "pwc.auto-sort",
                "Auto Sort",
                "0.6.1",
                "mod",
                "Sort containers with one click.",
                &["gameplay"],
                &[],
                &[],
            ),
            available(
                "pwc.lan-party",
                "LAN Party",
                "0.2.0",
                "mod",
                "Zero-config LAN discovery.",
                &["tools"],
                &[],
                &[],
            ),
            available(
                "pwc.weather-plus",
                "Weather Plus",
                "0.5.2",
                "mod",
                "Storms, fog banks and seasonal cycles.",
                &["worldgen", "visuals"],
                &[],
                &[],
            ),
        ];
        mods_catalog.extend(extra_catalog());

        let doctor = vec![
            Check {
                name: "Git".into(),
                level: Level::Ok,
                detail: "git version 2.51.0".into(),
                hint: String::new(),
            },
            Check {
                name: "Cargo and rustc".into(),
                level: Level::Ok,
                detail: "rustc 1.95.0 (abcdef123 2026-10-01)".into(),
                hint: String::new(),
            },
            Check {
                name: "Nix".into(),
                level: Level::Info,
                detail: "builds run inside the game's nix develop shell".into(),
                hint: String::new(),
            },
            Check {
                name: "Vulkan driver".into(),
                level: Level::Ok,
                detail: "found an ICD in /run/opengl-driver/share/vulkan/icd.d".into(),
                hint: String::new(),
            },
            Check {
                name: "Disk space".into(),
                level: Level::Warn,
                detail: "9.1 GiB free where the pwc cache lives".into(),
                hint: "builds need headroom; consider cleaning the build cache".into(),
            },
            Check {
                name: "Memory".into(),
                level: Level::Ok,
                detail: "32 GiB total".into(),
                hint: String::new(),
            },
        ];

        Self {
            settings: Settings::default(),
            instances,
            updates,
            mods_catalog,
            doctor,
            next_op_id: 1,
            stress_log_lines: 0,
        }
    }

    /// Like [`DemoBackend::new`], but with no instances (the welcome screen). Only the
    /// snapshot tests (a separate compilation unit) call this.
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self {
            instances: Vec::new(),
            updates: BTreeMap::new(),
            ..Self::new()
        }
    }

    /// A large fixture for the frame-cost harness: `instances` sidebar rows, `mods` catalogue
    /// entries, and a build op whose log already holds `log_lines` lines.
    ///
    /// Animations and the startup update check are off, so a stepped harness measures layout
    /// rather than a spinner. Only the perf test calls this.
    #[allow(dead_code)]
    pub fn stress(instances: usize, mods: usize, log_lines: usize) -> Self {
        let now = watt_core::paths::now_secs();
        let mut settings = Settings::default();
        settings.check_updates_on_start = false;
        settings.appearance.animations = false;
        Self {
            settings,
            instances: (0..instances).map(|i| stress_entry(i, now)).collect(),
            updates: BTreeMap::new(),
            mods_catalog: (0..mods).map(stress_mod).collect(),
            doctor: Vec::new(),
            next_op_id: 1,
            stress_log_lines: log_lines,
        }
    }
}

/// One managed instance with a single ready generation. Names are `s00`, `s01`, …
#[allow(dead_code)]
fn stress_entry(index: usize, now: u64) -> InstanceEntry {
    let name = format!("s{index:02}");
    let meta = Meta {
        format: 1,
        display_name: Some(format!("Instance {index}")),
        color: None,
        group: (index >= 3 && index % 5 == 0).then(|| "Bench".to_owned()),
        favorite: index < 3,
        notes: String::new(),
        created: Some(now.saturating_sub(86_400)),
        last_played: None,
        source: Binding::Managed {
            channel: "main".into(),
            engine_ref: watt_core::meta::AUTO.into(),
            mods_ref: watt_core::meta::AUTO.into(),
            pin: Some(pin("main", &name)),
        },
        generations: vec![generation(1, now, "0.6.1", &name, true)],
        last_op: None,
    };
    let last_good = meta.last_good().cloned();
    InstanceEntry {
        card: InstanceCard {
            name: name.clone(),
            dir: PathBuf::from(format!("/var/lib/pwc/instances/{name}")),
            manifest: InstanceManifest::new(
                InstanceName::parse(&name).expect("stress instance name"),
                VersionReq::STAR,
            ),
            meta,
            lock: None,
            packages: Vec::new(),
            game: GameInfo {
                version: Some("0.6.1".into()),
                api: Some("0.4.2".into()),
                revision: Some(fake_sha(&format!("{name}-rev"))),
                dirty: false,
                source_dir: Some(PathBuf::from(format!("/var/lib/pwc/sources/v1/{name}"))),
                branch: Some("main".into()),
                engine_revision: Some(fake_sha(&format!("{name}-engine"))),
                mods_revision: Some(fake_sha(&format!("{name}-mods"))),
                environment: Some(format!("sha256:{}", fake_sha(&name))),
            },
            active: index == 0,
            problems: Vec::new(),
        },
        status: InstanceStatus {
            readiness: Readiness::Ready {
                game: PathBuf::from(format!("/var/lib/pwc/cache/{name}/game")),
            },
            busy: false,
            running: None,
            last_good,
        },
    }
}

#[allow(dead_code)]
fn stress_mod(index: usize) -> AvailableMod {
    let id = format!("pwc.stress-{index}");
    let name = format!("Stress Mod {index}");
    available(
        &id,
        &name,
        "1.0.0",
        "mod",
        "Generated catalogue row for the frame-cost harness.",
        &["worldgen"],
        &[],
        &[],
    )
}

impl Backend for DemoBackend {
    fn settings(&self) -> &Settings {
        &self.settings
    }

    fn save_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    fn tick(&mut self) -> bool {
        false
    }

    fn instances(&self) -> &[InstanceEntry] {
        &self.instances
    }

    fn load_errors(&self) -> &[(String, String)] {
        &[]
    }

    fn updates_available(&self, instance: &str) -> Option<&[RepoLog]> {
        self.updates.get(instance).map(Vec::as_slice)
    }

    fn refresh(&mut self) {}

    fn edit_meta(&mut self, instance: &str, edit: MetaEdit) -> Result<Meta, String> {
        let entry = self
            .instances
            .iter_mut()
            .find(|e| e.card.name == instance)
            .ok_or_else(|| "instance not found".to_owned())?;
        match edit {
            MetaEdit::DisplayName(v) => {
                entry.card.meta.display_name = (!v.trim().is_empty()).then_some(v)
            }
            MetaEdit::Color(v) => entry.card.meta.color = (!v.trim().is_empty()).then_some(v),
            MetaEdit::Group(v) => entry.card.meta.group = (!v.trim().is_empty()).then_some(v),
            MetaEdit::Favorite(v) => entry.card.meta.favorite = v,
            MetaEdit::Notes(v) => entry.card.meta.notes = v,
        }
        Ok(entry.card.meta.clone())
    }

    fn available_mods(&mut self, _instance: &str) -> Loaded<(Vec<AvailableMod>, Vec<String>)> {
        Loaded::Ready((self.mods_catalog.clone(), Vec::new()))
    }

    fn branches(&mut self) -> Loaded<Vec<BranchInfo>> {
        Loaded::Ready(vec![
            BranchInfo {
                name: "main".into(),
                sha: fake_sha("branch-main"),
                time: 1_760_000_000,
                summary: "Release 0.6.1".into(),
            },
            BranchInfo {
                name: "experimental".into(),
                sha: fake_sha("branch-experimental"),
                time: 1_759_900_000,
                summary: "Zeldovich law candidates".into(),
            },
            BranchInfo {
                name: "mp-fix".into(),
                sha: fake_sha("branch-mp-fix"),
                time: 1_759_800_000,
                summary: "Multiplayer desync fix".into(),
            },
        ])
    }

    fn doctor(&mut self) -> Loaded<Vec<Check>> {
        Loaded::Ready(self.doctor.clone())
    }

    fn run_doctor(&mut self) {}

    fn start_op(&mut self, request: OpRequest) -> RunningOp {
        let op_id = format!("demo-{}", self.next_op_id);
        self.next_op_id += 1;
        if self.stress_log_lines > 0 {
            return RunningOp::Demo(DemoOp::stress(op_id, &request, self.stress_log_lines));
        }
        let fail = request.instance() == Some("flaky-build");
        RunningOp::Demo(DemoOp::new(op_id, &request, fail))
    }

    fn open_path(&self, _path: &Path) {}
}

/// A timed sequence of envelopes that plays back like a real op.
pub struct DemoOp {
    op_id: String,
    started: Instant,
    timeline: Vec<(u64, Event)>,
    next: usize,
    cancelled: bool,
    finished: bool,
}

impl DemoOp {
    /// Build a realistic timeline for `request`.
    pub fn new(op_id: String, request: &OpRequest, fail: bool) -> Self {
        let instance = request.instance().map(str::to_owned);
        let kind = request.kind();
        let timeline = match kind {
            OpKind::Play => play_timeline(instance),
            OpKind::CheckUpdates => check_updates_timeline(instance),
            OpKind::Maintenance => maintenance_timeline(),
            OpKind::Remove | OpKind::Duplicate => quick_timeline(kind, instance),
            _ => build_timeline(kind, instance, fail),
        };
        Self {
            op_id,
            started: Instant::now(),
            timeline,
            next: 0,
            cancelled: false,
            finished: false,
        }
    }

    /// An op that failed before it could even start (e.g. `current_exe` was unavailable). Only
    /// `RealBackend::start_op`'s fallback path constructs this, so it is unused when the
    /// snapshot tests' separate compilation unit only exercises `DemoBackend`.
    #[allow(dead_code)]
    pub fn immediate_failure(kind: OpKind, instance: Option<String>, error: String) -> Self {
        let timeline = vec![
            (
                0,
                Event::OpStarted {
                    op: kind,
                    instance,
                    phases: vec![Phase::Prepare],
                    log: None,
                },
            ),
            (
                0,
                Event::OpFinished {
                    outcome: OpOutcome::Failed {
                        phase: Some(Phase::Prepare),
                        error,
                        recovery: vec![Recovery::Retry],
                    },
                    duration_ms: 0,
                },
            ),
        ];
        Self {
            op_id: "demo-immediate".to_owned(),
            started: Instant::now(),
            timeline,
            next: 0,
            cancelled: false,
            finished: false,
        }
    }

    /// Apply every envelope due by now.
    pub fn poll(&mut self, view: &mut OpView) -> bool {
        if self.finished {
            return false;
        }
        let elapsed = self.started.elapsed().as_millis() as u64;
        if self.cancelled {
            let env = Envelope {
                protocol: PROTOCOL,
                op_id: self.op_id.clone(),
                seq: self.next as u64,
                t_ms: elapsed,
                event: Event::OpFinished {
                    outcome: OpOutcome::Cancelled,
                    duration_ms: elapsed,
                },
            };
            view.apply(&env);
            self.finished = true;
            return true;
        }
        let mut changed = false;
        while self.next < self.timeline.len() && self.timeline[self.next].0 <= elapsed {
            let (t_ms, event) = self.timeline[self.next].clone();
            let is_finish = matches!(event, Event::OpFinished { .. });
            let env = Envelope {
                protocol: PROTOCOL,
                op_id: self.op_id.clone(),
                seq: self.next as u64,
                t_ms,
                event,
            };
            view.apply(&env);
            changed = true;
            self.next += 1;
            if is_finish {
                self.finished = true;
            }
        }
        changed
    }

    /// Ask the (simulated) op to stop.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// A build that is already in Compile, with `log_lines` tool lines due immediately, and
    /// that never finishes. The frame-cost harness opens the Forge and the Log tab on it.
    #[allow(dead_code)]
    pub fn stress(op_id: String, request: &OpRequest, log_lines: usize) -> Self {
        let instance = request.instance().map(str::to_owned);
        let mut timeline = vec![
            (0, started(OpKind::Build, instance, &[Phase::Compile])),
            (0, phase(Phase::Compile, "Compiling")),
            (
                0,
                Event::Progress {
                    phase: Phase::Compile,
                    done: 40,
                    total: Some(100),
                    detail: "watt_stress".into(),
                },
            ),
        ];
        timeline.reserve(log_lines);
        for i in 0..log_lines {
            timeline.push((
                0,
                Event::Log {
                    stream: LogStream::Tool,
                    line: format!("compiling crate {i}"),
                },
            ));
        }
        Self {
            op_id,
            started: Instant::now(),
            timeline,
            next: 0,
            cancelled: false,
            finished: false,
        }
    }
}

fn started(op: OpKind, instance: Option<String>, phases: &[Phase]) -> Event {
    Event::OpStarted {
        op,
        instance,
        phases: phases.to_vec(),
        log: None,
    }
}

fn phase(phase: Phase, title: &str) -> Event {
    Event::PhaseStarted {
        phase,
        title: title.to_owned(),
        determinate: matches!(phase, Phase::Compile),
    }
}

fn phase_done(phase: Phase, duration_ms: u64) -> Event {
    Event::PhaseFinished {
        phase,
        outcome: PhaseOutcome::Ok,
        duration_ms,
    }
}

/// The realistic success path: Prepare, Fetch, Checkout, Lock, Toolchain, a long Compile with
/// changing crate names, Link, Install, then success (or a failure during Lock).
fn build_timeline(kind: OpKind, instance: Option<String>, fail: bool) -> Vec<(u64, Event)> {
    let phases = vec![
        Phase::Prepare,
        Phase::Fetch,
        Phase::Checkout,
        Phase::Lock,
        Phase::Toolchain,
        Phase::Compile,
        Phase::Link,
        Phase::Install,
    ];
    let mut t: Vec<(u64, Event)> = vec![
        (0, started(kind, instance.clone(), &phases)),
        (0, phase(Phase::Prepare, "Getting ready")),
        (150, phase_done(Phase::Prepare, 150)),
        (150, phase(Phase::Fetch, "Downloading updates")),
        (900, phase_done(Phase::Fetch, 750)),
        (900, phase(Phase::Checkout, "Unpacking sources")),
        (1400, phase_done(Phase::Checkout, 500)),
    ];

    if fail {
        t.push((1400, phase(Phase::Lock, "Resolving mods")));
        t.push((
            2100,
            Event::Warning {
                message: "pwc.hotbar requires pwc-mod-api ^0.5, found 0.4.2".into(),
            },
        ));
        t.push((
            2100,
            Event::OpFinished {
                outcome: OpOutcome::Failed {
                    phase: Some(Phase::Lock),
                    error: "resolving mods: pwc.hotbar requires pwc-mod-api ^0.5, found 0.4.2"
                        .into(),
                    recovery: vec![Recovery::ResetLock, Recovery::PlayLastGood],
                },
                duration_ms: 2100,
            },
        ));
        return t;
    }

    t.push((1400, phase(Phase::Lock, "Resolving mods")));
    t.push((
        1400,
        Event::Changed {
            change: Change::Lock {
                changes: vec![watt_core::events::PackageChange {
                    id: "pwc.hotbar".into(),
                    old: Some("2.0.0".into()),
                    new: Some("2.1.0".into()),
                }],
                environment: "sha256:demo".into(),
            },
        },
    ));
    t.push((1700, phase_done(Phase::Lock, 300)));
    t.push((1700, phase(Phase::Toolchain, "Waking up the toolchain")));
    t.push((1900, phase_done(Phase::Toolchain, 200)));
    t.push((1900, phase(Phase::Compile, "Compiling")));

    let crates = [
        "serde",
        "wgpu",
        "cgmath",
        "voxel_engine",
        "pwc_hotbar",
        "project_watt_cubed",
    ];
    let total = crates.len() as u64 * 40;
    let mut t_ms = 1900u64;
    for (i, name) in crates.iter().enumerate() {
        t_ms += 1500;
        t.push((
            t_ms,
            Event::Progress {
                phase: Phase::Compile,
                done: (i as u64 + 1) * 40,
                total: Some(total),
                detail: name.to_string(),
            },
        ));
    }
    let compile_end = t_ms + 300;
    t.push((compile_end, phase_done(Phase::Compile, compile_end - 1900)));
    t.push((compile_end, phase(Phase::Link, "Linking the game")));
    let link_end = compile_end + 4500;
    t.push((link_end, phase_done(Phase::Link, link_end - compile_end)));
    t.push((link_end, phase(Phase::Install, "Installing the build")));
    t.push((
        link_end,
        Event::Changed {
            change: Change::Build {
                build_id: "demo-build".into(),
                cached: false,
                game: PathBuf::from("/var/lib/pwc/cache/demo/game"),
            },
        },
    ));
    let install_end = link_end + 300;
    t.push((install_end, phase_done(Phase::Install, 300)));
    t.push((
        install_end,
        Event::Changed {
            change: Change::Generation { id: 4 },
        },
    ));
    t.push((
        install_end,
        Event::OpFinished {
            outcome: OpOutcome::Ok {
                summary: "Built 0.6.1, 1 mod updated".into(),
            },
            duration_ms: install_end,
        },
    ));
    t
}

fn play_timeline(instance: Option<String>) -> Vec<(u64, Event)> {
    vec![
        (
            0,
            started(OpKind::Play, instance, &[Phase::Prepare, Phase::Launch]),
        ),
        (0, phase(Phase::Prepare, "Getting ready")),
        (200, phase_done(Phase::Prepare, 200)),
        (200, phase(Phase::Launch, "Launching")),
        (
            500,
            Event::Changed {
                change: Change::Launched {
                    pid: 123_456,
                    log: PathBuf::from("/var/lib/pwc/instances/demo/watt-start/logs/game.log"),
                },
            },
        ),
        (500, phase_done(Phase::Launch, 300)),
        (
            500,
            Event::OpFinished {
                outcome: OpOutcome::Ok {
                    summary: "Started the game".into(),
                },
                duration_ms: 500,
            },
        ),
    ]
}

fn check_updates_timeline(instance: Option<String>) -> Vec<(u64, Event)> {
    let name = instance.clone().unwrap_or_else(|| "frontier".to_owned());
    vec![
        (0, started(OpKind::CheckUpdates, instance, &[Phase::Fetch])),
        (0, phase(Phase::Fetch, "Downloading updates")),
        (
            600,
            Event::Changed {
                change: Change::UpdatesAvailable {
                    instance: name,
                    whats_new: vec![RepoLog {
                        repo: Repo::Game,
                        commits: vec![Commit {
                            sha: fake_sha("check-updates"),
                            summary: "Tune drive coefficients".into(),
                            author: "gusahlg".into(),
                            time: 1_760_000_000,
                        }],
                        truncated: false,
                    }],
                },
            },
        ),
        (600, phase_done(Phase::Fetch, 600)),
        (
            600,
            Event::OpFinished {
                outcome: OpOutcome::Ok {
                    summary: "1 commit waiting".into(),
                },
                duration_ms: 600,
            },
        ),
    ]
}

fn maintenance_timeline() -> Vec<(u64, Event)> {
    vec![
        (0, started(OpKind::Maintenance, None, &[Phase::Cleanup])),
        (0, phase(Phase::Cleanup, "Cleaning up")),
        (800, phase_done(Phase::Cleanup, 800)),
        (
            800,
            Event::OpFinished {
                outcome: OpOutcome::Ok {
                    summary: "Freed 2.3 GiB".into(),
                },
                duration_ms: 800,
            },
        ),
    ]
}

fn quick_timeline(kind: OpKind, instance: Option<String>) -> Vec<(u64, Event)> {
    vec![
        (0, started(kind, instance, &[Phase::Prepare])),
        (0, phase(Phase::Prepare, "Getting ready")),
        (300, phase_done(Phase::Prepare, 300)),
        (
            300,
            Event::OpFinished {
                outcome: OpOutcome::Ok {
                    summary: "Done".into(),
                },
                duration_ms: 300,
            },
        ),
    ]
}
