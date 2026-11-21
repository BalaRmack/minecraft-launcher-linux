//! Opt-in live integration probe. Downloads real game files into a temporary directory.
use crate::{catalog, integrity, java, modrinth, offline_uuid};
use mc_launcher_core::prelude::*;
use std::{
    fs,
    process::{Command, Stdio},
};

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
    let root = temp.path().join("minecraft");
    let launcher = Launcher::new(&root);
    let vanilla = launcher.install(InstallRequest::vanilla("1.20.1")).unwrap();
    let mut skipped = 0usize;
    launcher
        .install_with_progress(InstallRequest::vanilla("1.20.1"), &mut |event| {
            if matches!(event, ProgressEvent::TaskSkipped { .. }) {
                skipped += 1;
            }
        })
        .unwrap();
    assert!(
        skipped > 100,
        "verified libraries and assets should be reused"
    );
    let fabric = launcher
        .install_with_progress_and_java(
            InstallRequest {
                minecraft_version: "1.20.1".into(),
                loader: Some(LoaderSpec::Fabric {
                    version: LoaderVersion::LatestStable,
                }),
                java: JavaInstallPolicy::Never,
            },
            &mut |_e| {},
            &java,
        )
        .unwrap();
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
            let count = modrinth::install(&game.join("mods"), "sodium", "1.20.1", "fabric", |s| {
                eprintln!("{s}")
            })
            .unwrap();
            assert!(count > 0);
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
