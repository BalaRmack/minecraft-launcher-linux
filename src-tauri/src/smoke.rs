//! Opt-in live integration probe. Downloads real game files into a temporary directory.
use crate::{catalog, copy_instance, integrity, java, loaders, mod_state, modrinth, offline_uuid};
use mc_launcher_core::prelude::*;
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::Instant,
};
struct Metrics {
    cache: PathBuf,
    started: Instant,
    downloads: usize,
    reused: usize,
    bytes: u64,
    current: HashMap<String, u64>,
}
impl Metrics {
    fn new(cache: PathBuf) -> Self {
        Self {
            cache,
            started: Instant::now(),
            downloads: 0,
            reused: 0,
            bytes: 0,
            current: HashMap::new(),
        }
    }
    fn print(&self, name: &str) {
        let secs = self.started.elapsed().as_secs_f64();
        eprintln!(
            "::notice title=Horizon benchmark::{name}: {:.2}s downloaded={} reused={} network_bytes={} avg_MB_s={:.2}",
            secs,
            self.downloads,
            self.reused,
            self.bytes,
            self.bytes as f64 / secs.max(0.01) / 1_000_000.0
        );
    }
}
impl ProgressReporter for Metrics {
    fn report(&mut self, e: ProgressEvent) {
        match e {
            ProgressEvent::BatchStarted { .. } => self.current.clear(),
            ProgressEvent::TaskFinished { .. } => self.downloads += 1,
            ProgressEvent::TaskSkipped { .. } => self.reused += 1,
            ProgressEvent::BytesReceived {
                label, received, ..
            } => {
                let before = self.current.insert(label, received).unwrap_or(0);
                self.bytes += if received >= before {
                    received - before
                } else {
                    received
                };
            }
            _ => (),
        }
    }
    fn cache_dir(&self) -> Option<PathBuf> {
        Some(self.cache.clone())
    }
}

pub fn run() {
    use std::time::{Duration, Instant};
    // Flatpak's /tmp can be a small tmpfs: several isolated installations plus
    // the shared cache exceed it. Use the sandbox's writable disk-backed cache.
    let temp = if std::path::Path::new("/.flatpak-info").exists() {
        let cache =
            PathBuf::from(std::env::var_os("XDG_CACHE_HOME").expect("Flatpak XDG_CACHE_HOME"));
        fs::create_dir_all(&cache).unwrap();
        tempfile::Builder::new()
            .prefix("horizon-smoke-")
            .tempdir_in(cache)
            .unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    // Make the live test exercise the managed runtime even on a runner
    // that already has Java 17 installed. This test runs alone.
    let old_path = std::env::var_os("PATH");
    std::env::set_var("PATH", "/nonexistent");
    let java_result = java::resolve(temp.path(), "auto", 17, |s| eprintln!("{s}"));
    if let Some(path) = old_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let java = java_result.unwrap();
    assert!(java.starts_with(temp.path().join("runtime")));
    let reused_java = java::resolve(temp.path(), java.to_str().unwrap(), 17, |_| {}).unwrap();
    assert_eq!(java, reused_java);
    let cache = temp.path().join("shared-cache");
    fs::create_dir(&cache).unwrap();
    let legacy_root = temp.path().join("legacy");
    let legacy_launcher = Launcher::new(&legacy_root);
    let java8 = java::resolve(temp.path(), "auto", 8, |s| eprintln!("{s}")).unwrap();
    let mut first = Metrics::new(cache.clone());
    let legacy = legacy_launcher
        .install_with_progress_and_java(InstallRequest::vanilla("1.10"), &mut first, &java8)
        .unwrap();
    first.print("vanilla-1.10-first");
    let mut repeat = Metrics::new(cache.clone());
    legacy_launcher
        .install_with_progress_and_java(InstallRequest::vanilla("1.10"), &mut repeat, &java8)
        .unwrap();
    repeat.print("vanilla-1.10-repeat");
    assert!(repeat.reused > 100);
    let second_root = temp.path().join("legacy-second-instance");
    let mut shared = Metrics::new(cache.clone());
    Launcher::new(&second_root)
        .install_with_progress_and_java(InstallRequest::vanilla("1.10"), &mut shared, &java8)
        .unwrap();
    shared.print("vanilla-1.10-shared-cache");
    assert!(shared.reused > 100);
    let legacy_game = temp.path().join("legacy-game");
    fs::create_dir_all(&legacy_game).unwrap();
    let legacy_version = legacy_launcher.load_version(&legacy.version_id).unwrap();
    let legacy_command = legacy_launcher
        .build_launch_command_from_version(
            &legacy_version,
            LaunchOptions {
                account: Account::Offline {
                    username: "HorizonSmoke".into(),
                    uuid: offline_uuid("HorizonSmoke"),
                },
                java_executable: Some(java8),
                game_directory: Some(legacy_game.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let out = fs::File::create(legacy_game.join("process.log")).unwrap();
    let mut old = Command::new(legacy_command.executable)
        .arg("-Xmx1024M")
        .args(legacy_command.args)
        .envs(legacy_command.env)
        .current_dir(legacy_command.working_dir)
        .stdout(Stdio::from(out.try_clone().unwrap()))
        .stderr(Stdio::from(out))
        .spawn()
        .unwrap();
    let start = Instant::now();
    let old_success = loop {
        let log = fs::read_to_string(legacy_game.join("logs/latest.log")).unwrap_or_default();
        if log.contains("Reloading ResourceManager") {
            break true;
        }
        if old.try_wait().unwrap().is_some() || start.elapsed() > Duration::from_secs(180) {
            break false;
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    let _ = old.kill();
    let _ = old.wait();
    if !old_success {
        panic!(
            "Minecraft 1.10 failed to initialize graphics: {}",
            fs::read_to_string(legacy_game.join("process.log")).unwrap_or_default()
        );
    }
    let root = temp.path().join("minecraft");
    let launcher = Launcher::new(&root);
    let mut modern_first = Metrics::new(cache.clone());
    let vanilla = launcher
        .install_with_progress_and_java(InstallRequest::vanilla("1.20.1"), &mut modern_first, &java)
        .unwrap();
    modern_first.print("vanilla-1.20.1-first");
    let mut modern_repeat = Metrics::new(cache.clone());
    launcher
        .install_with_progress_and_java(
            InstallRequest::vanilla("1.20.1"),
            &mut modern_repeat,
            &java,
        )
        .unwrap();
    modern_repeat.print("vanilla-1.20.1-repeat");
    assert!(
        modern_repeat.reused > 100,
        "verified libraries and assets should be reused"
    );
    let mut fabric_metrics = Metrics::new(cache.clone());
    let fabric = launcher
        .install_with_progress_and_java(
            InstallRequest {
                minecraft_version: "1.20.1".into(),
                loader: Some(LoaderSpec::Fabric {
                    version: LoaderVersion::LatestStable,
                }),
                java: JavaInstallPolicy::Never,
            },
            &mut fabric_metrics,
            &java,
        )
        .unwrap();
    fabric_metrics.print("fabric-1.20.1");
    for (number, installed) in [vanilla, fabric].into_iter().enumerate() {
        integrity::verify(&root, &installed.version_id).unwrap();
        if number == 0 {
            let jar = root.join("versions/1.20.1/1.20.1.jar");
            let backup = root.join("client-smoke-backup.jar");
            fs::rename(&jar, &backup).unwrap();
            assert!(integrity::verify(&root, &installed.version_id).is_err());
            // Reinstallation must restore the missing file without deleting unrelated data.
            launcher.install(InstallRequest::vanilla("1.20.1")).unwrap();
            assert!(jar.is_file());
            assert!(backup.is_file());
            integrity::verify(&root, &installed.version_id).unwrap();
        }
        let game = temp.path().join(format!("game-{number}"));
        fs::create_dir_all(&game).unwrap();
        if number == 1 {
            let mod_cache = temp.path().join("mod-cache");
            fs::create_dir(&mod_cache).unwrap();
            let count = modrinth::install_with_cache(
                &game.join("mods"),
                "sodium",
                "1.20.1",
                "fabric",
                Some(&mod_cache),
                |s| eprintln!("{s}"),
            )
            .unwrap();
            assert!(count > 0);
            let from_cache = std::sync::atomic::AtomicBool::new(false);
            let second_mods = temp.path().join("mod-cache-second-instance");
            modrinth::install_with_cache(
                &second_mods,
                "sodium",
                "1.20.1",
                "fabric",
                Some(&mod_cache),
                |s| {
                    if s.starts_with("Из кэша:") {
                        from_cache.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                },
            )
            .unwrap();
            assert!(from_cache.load(std::sync::atomic::Ordering::Relaxed));
            let mods_started = Instant::now();
            let lithium =
                modrinth::install(&game.join("mods"), "lithium", "1.20.1", "fabric", |_| {})
                    .unwrap();
            assert!(lithium > 0);
            eprintln!(
                "BENCHMARK fabric-second-mod: {:.2}s files={lithium}",
                mods_started.elapsed().as_secs_f64()
            );
            let cache = catalog::Cache::default();
            let search = catalog::search(
                &cache,
                catalog::Search {
                    query: "sodium".into(),
                    version: "1.20.1".into(),
                    loader: "fabric".into(),
                    category: "".into(),
                    sort: "relevance".into(),
                    offset: 0,
                },
            )
            .unwrap();
            assert!(search["hits"].as_array().is_some_and(|a| !a.is_empty()));
            let detail = catalog::details(&cache, "sodium", "1.20.1", "fabric").unwrap();
            assert!(detail["compatible_version"].is_object());
            let inspected =
                catalog::inspect(&cache, &game.join("mods"), "1.20.1", "fabric").unwrap();
            assert!(inspected
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["project"]["slug"] == "sodium"));
            assert_eq!(
                modrinth::install(&game.join("mods"), "sodium", "1.20.1", "fabric", |_| {})
                    .unwrap(),
                0
            );
        }
        let version = launcher.load_version(&installed.version_id).unwrap();
        let command = launcher
            .build_launch_command_from_version(
                &version,
                LaunchOptions {
                    account: Account::Offline {
                        username: "HorizonSmoke".into(),
                        uuid: offline_uuid("HorizonSmoke"),
                    },
                    java_executable: Some(java.clone()),
                    game_directory: Some(game.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let output = fs::File::create(game.join("process.log")).unwrap();
        let mut child = Command::new(command.executable)
            .arg("-Xmx1024M")
            .args(command.args)
            .envs(command.env)
            .current_dir(command.working_dir)
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap();
        let start = Instant::now();
        let success = loop {
            let log = fs::read_to_string(game.join("logs/latest.log")).unwrap_or_default();
            if log.contains("Backend library: LWJGL") {
                break true;
            }
            if child.try_wait().unwrap().is_some() || start.elapsed() > Duration::from_secs(180) {
                break false;
            }
            std::thread::sleep(Duration::from_secs(1));
        };
        let _ = child.kill();
        let _ = child.wait();
        if !success {
            panic!(
                "Minecraft failed to reach LWJGL: {}",
                fs::read_to_string(game.join("process.log")).unwrap_or_default()
            );
        }
    }
    // Exercise Xaero by its canonical Modrinth ID, including an actual old-to-new update.
    let result = catalog::search(
        &catalog::Cache::default(),
        catalog::Search {
            query: "Xaero's Minimap".into(),
            version: "1.20.1".into(),
            loader: "fabric".into(),
            category: "".into(),
            sort: "relevance".into(),
            offset: 0,
        },
    )
    .unwrap();
    let xaero = result["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["slug"] == "xaeros-minimap")
        .expect("Xaero search result")["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let versions: serde_json::Value = crate::client()
        .unwrap()
        .get(format!(
            "https://api.modrinth.com/v2/project/{xaero}/version"
        ))
        .query(&[
            ("game_versions", "[\"1.20.1\"]"),
            ("loaders", "[\"fabric\"]"),
        ])
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    let versions = versions.as_array().unwrap();
    let newest = &versions[0];
    let older = versions
        .iter()
        .find(|v| {
            v["id"] != newest["id"]
                && v["date_published"].as_str() < newest["date_published"].as_str()
                && v["dependencies"]
                    .as_array()
                    .is_some_and(|d| d.iter().all(|x| x["dependency_type"] != "required"))
        })
        .expect("older Xaero release");
    let xaero_dir = temp.path().join("xaero-profile/game/mods");
    fs::create_dir_all(&xaero_dir).unwrap();
    modrinth::install_exact_for_test(
        &xaero_dir,
        &xaero,
        older["id"].as_str().unwrap(),
        "1.20.1",
        "fabric",
    )
    .unwrap();
    let before = mod_state::status(&xaero_dir, "1.20.1", "fabric").unwrap();
    let row = before["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["version"]["project_id"] == xaero)
        .unwrap();
    assert_eq!(row["version"]["id"], older["id"]);
    assert_eq!(row["update"]["id"], newest["id"]);
    let old_name = row["name"].as_str().unwrap().to_owned();
    let old_bytes = fs::read(xaero_dir.join(&old_name)).unwrap();
    assert_eq!(
        modrinth::install(&xaero_dir, &xaero, "1.20.1", "fabric", |_| {}).unwrap(),
        0
    );
    modrinth::update_with_cache(&xaero_dir, &xaero, "1.20.1", "fabric", None, |_| {}).unwrap();
    let after = mod_state::status(&xaero_dir, "1.20.1", "fabric").unwrap();
    let row = after["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["version"]["project_id"] == xaero)
        .unwrap();
    assert_eq!(row["version"]["id"], newest["id"]);
    let backup_root = xaero_dir.parent().unwrap().join(".horizon-mod-backups");
    let backup = fs::read_dir(backup_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join(old_name);
    assert_eq!(fs::read(backup).unwrap(), old_bytes);
    let second = temp.path().join("xaero-second/game/mods");
    fs::create_dir_all(&second).unwrap();
    modrinth::install(&second, &xaero, "1.20.1", "fabric", |_| {}).unwrap();
    fs::remove_file(xaero_dir.join(row["name"].as_str().unwrap())).unwrap();
    let removed = mod_state::status(&xaero_dir, "1.20.1", "fabric").unwrap();
    assert!(!removed["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["version"]["project_id"] == xaero));
    assert!(
        mod_state::status(&second, "1.20.1", "fabric").unwrap()["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["version"]["project_id"] == xaero)
    );

    // Exercise real isolated copies and exact loader installs, without ever editing the source.
    let game = root.join("game");
    fs::create_dir_all(game.join("saves/world")).unwrap();
    fs::create_dir_all(game.join("mods")).unwrap();
    fs::write(game.join("saves/world/marker"), b"world data").unwrap();
    fs::write(game.join("mods/example.jar.disabled"), b"mod data").unwrap();
    fs::write(game.join("options.txt"), b"settings").unwrap();
    let mut source = root.clone();
    for kind in ["forge", "fabric", "vanilla"] {
        let dest = temp.path().join(format!("switch-{kind}"));
        copy_instance(&source, &dest).unwrap();
        let loader = match kind {
            "forge" => Some(LoaderSpec::Forge {
                version: LoaderVersion::Exact(
                    loaders::versions("1.20.1", kind)
                        .unwrap()
                        .first()
                        .expect("Forge version")
                        .clone(),
                ),
            }),
            "fabric" => Some(LoaderSpec::Fabric {
                version: LoaderVersion::Exact(
                    loaders::versions("1.20.1", kind)
                        .unwrap()
                        .first()
                        .expect("Fabric version")
                        .clone(),
                ),
            }),
            _ => None,
        };
        let target = Launcher::new(&dest);
        let mut metrics = Metrics::new(cache.clone());
        let installed = target
            .install_with_progress_and_java(
                InstallRequest {
                    minecraft_version: "1.20.1".into(),
                    loader,
                    java: JavaInstallPolicy::Never,
                },
                &mut metrics,
                &java,
            )
            .unwrap();
        integrity::verify(&dest, &installed.version_id).unwrap();
        for dir in [&root, &dest] {
            assert_eq!(
                fs::read(dir.join("game/saves/world/marker")).unwrap(),
                b"world data"
            );
            assert_eq!(
                fs::read(dir.join("game/mods/example.jar.disabled")).unwrap(),
                b"mod data"
            );
            assert_eq!(fs::read(dir.join("game/options.txt")).unwrap(), b"settings");
        }
        let version = target.load_version(&installed.version_id).unwrap();
        let command = target
            .build_launch_command_from_version(
                &version,
                LaunchOptions {
                    account: Account::Offline {
                        username: "HorizonSmoke".into(),
                        uuid: offline_uuid("HorizonSmoke"),
                    },
                    java_executable: Some(java.clone()),
                    game_directory: Some(dest.join("game")),
                    ..Default::default()
                },
            )
            .unwrap();
        let _ = fs::remove_dir_all(dest.join("game/logs"));
        let log_file = fs::File::create(dest.join("game/switch-process.log")).unwrap();
        let mut child = Command::new(command.executable)
            .arg("-Xmx1024M")
            .args(command.args)
            .envs(command.env)
            .current_dir(command.working_dir)
            .stdout(Stdio::from(log_file.try_clone().unwrap()))
            .stderr(Stdio::from(log_file))
            .spawn()
            .unwrap();
        let start = Instant::now();
        let success = loop {
            let log = fs::read_to_string(dest.join("game/logs/latest.log")).unwrap_or_default();
            if log.contains("Backend library: LWJGL") {
                break true;
            }
            if child.try_wait().unwrap().is_some() || start.elapsed() > Duration::from_secs(180) {
                break false;
            }
            std::thread::sleep(Duration::from_secs(1));
        };
        let _ = child.kill();
        let _ = child.wait();
        if !success {
            panic!(
                "{kind} switch failed to start: {}",
                fs::read_to_string(dest.join("game/switch-process.log")).unwrap_or_default()
            );
        }
        if source != root {
            fs::remove_dir_all(&source).unwrap();
        }
        source = dest;
    }
}
