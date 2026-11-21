//! Opt-in live integration probe. Downloads real game files into a temporary directory.
use crate::{catalog, integrity, java, modrinth, offline_uuid};
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
    let temp = tempfile::tempdir().unwrap();
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
}
