#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod java;
use mc_launcher_core::prelude::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Mutex,
};
use tauri::{Emitter, Manager, State};
use uuid::Uuid;

type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[derive(Default)]
struct Tasks(Mutex<HashSet<String>>);
#[derive(Default)]
struct RuntimeLock(Mutex<()>);
#[derive(Serialize, Deserialize, Clone)]
struct AccountRow {
    id: String,
    name: String,
}
#[derive(Serialize, Deserialize, Clone)]
struct Profile {
    id: String,
    name: String,
    version: String,
    loader: String,
    installed_id: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Settings {
    java: String,
    memory_mb: u32,
}
#[derive(Serialize)]
struct Snapshot {
    accounts: Vec<AccountRow>,
    profiles: Vec<Profile>,
    active_account: Option<String>,
    active_profile: Option<String>,
    settings: Settings,
}
fn root(app: &tauri::AppHandle) -> Result<PathBuf> {
    let p = app.path().app_data_dir().map_err(err)?;
    fs::create_dir_all(&p).map_err(err)?;
    Ok(p)
}
fn db(app: &tauri::AppHandle) -> Result<Connection> {
    let c = Connection::open(root(app)?.join("horizon.sqlite3")).map_err(err)?;
    c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS accounts(id TEXT PRIMARY KEY,name TEXT NOT NULL); CREATE TABLE IF NOT EXISTS profiles(id TEXT PRIMARY KEY,name TEXT NOT NULL,version TEXT NOT NULL,loader TEXT NOT NULL,installed_id TEXT); CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);").map_err(err)?;
    Ok(c)
}
fn get(c: &Connection, k: &str) -> Result<Option<String>> {
    c.query_row("SELECT value FROM settings WHERE key=?1", [k], |r| r.get(0))
        .optional()
        .map_err(err)
}
fn set(c: &Connection, k: &str, v: &str) -> Result<()> {
    c.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![k,v]).map_err(err)?;
    Ok(())
}
fn profile(c: &Connection, id: &str) -> Result<Profile> {
    c.query_row(
        "SELECT id,name,version,loader,installed_id FROM profiles WHERE id=?1",
        [id],
        |r| {
            Ok(Profile {
                id: r.get(0)?,
                name: r.get(1)?,
                version: r.get(2)?,
                loader: r.get(3)?,
                installed_id: r.get(4)?,
            })
        },
    )
    .map_err(err)
}
fn profile_dir(app: &tauri::AppHandle, id: &str) -> Result<PathBuf> {
    Uuid::parse_str(id).map_err(err)?;
    Ok(root(app)?.join("instances").join(id))
}
fn offline_uuid(name: &str) -> String {
    let mut bytes = md5::compute(format!("OfflinePlayer:{name}")).0;
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 64 && !name.chars().any(char::is_control)
}
fn valid_username(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
#[tauri::command]
fn snapshot(app: tauri::AppHandle) -> Result<Snapshot> {
    let c = db(&app)?;
    let accounts = c
        .prepare("SELECT id,name FROM accounts ORDER BY rowid")
        .map_err(err)?
        .query_map([], |r| {
            Ok(AccountRow {
                id: r.get(0)?,
                name: r.get(1)?,
            })
        })
        .map_err(err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(err)?;
    let profiles = c
        .prepare("SELECT id,name,version,loader,installed_id FROM profiles ORDER BY rowid")
        .map_err(err)?
        .query_map([], |r| {
            Ok(Profile {
                id: r.get(0)?,
                name: r.get(1)?,
                version: r.get(2)?,
                loader: r.get(3)?,
                installed_id: r.get(4)?,
            })
        })
        .map_err(err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(Snapshot {
        accounts,
        profiles,
        active_account: get(&c, "active_account")?,
        active_profile: get(&c, "active_profile")?,
        settings: Settings {
            java: get(&c, "java")?.unwrap_or_else(|| "auto".into()),
            memory_mb: get(&c, "memory_mb")?
                .and_then(|x| x.parse().ok())
                .unwrap_or(2048),
        },
    })
}
#[tauri::command]
fn save_account(app: tauri::AppHandle, id: Option<String>, name: String) -> Result<()> {
    if !valid_username(&name) {
        return Err(
            "Имя должно содержать 1–16 латинских букв, цифр или символов подчёркивания".into(),
        );
    }
    let c = db(&app)?;
    if let Some(id) = id {
        if c.execute("UPDATE accounts SET name=?1 WHERE id=?2", params![name, id])
            .map_err(err)?
            == 0
        {
            return Err("Аккаунт не найден".into());
        }
    } else {
        let id = Uuid::new_v4().to_string();
        c.execute("INSERT INTO accounts VALUES(?1,?2)", params![id, name])
            .map_err(err)?;
        set(&c, "active_account", &id)?;
    }
    Ok(())
}
#[tauri::command]
fn delete_account(app: tauri::AppHandle, id: String) -> Result<()> {
    let c = db(&app)?;
    c.execute("DELETE FROM accounts WHERE id=?1", [&id])
        .map_err(err)?;
    if get(&c, "active_account")?.as_deref() == Some(&id) {
        c.execute("DELETE FROM settings WHERE key='active_account'", [])
            .map_err(err)?;
    }
    Ok(())
}
#[tauri::command]
fn select_item(app: tauri::AppHandle, kind: String, id: String) -> Result<()> {
    let c = db(&app)?;
    let (table, key) = match kind.as_str() {
        "account" => ("accounts", "active_account"),
        "profile" => ("profiles", "active_profile"),
        _ => return Err("Неизвестный тип".into()),
    };
    let exists: bool = c
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)"),
            [&id],
            |r| r.get(0),
        )
        .map_err(err)?;
    if !exists {
        return Err("Объект не найден".into());
    }
    set(&c, key, &id)
}
#[tauri::command]
fn create_profile(
    app: tauri::AppHandle,
    name: String,
    version: String,
    loader: String,
) -> Result<()> {
    if !valid_name(&name)
        || version.is_empty()
        || version.starts_with('.')
        || version.contains("..")
        || version.len() > 80
        || !version
            .bytes()
            .all(|x| x.is_ascii_alphanumeric() || b"._-".contains(&x))
        || !["vanilla", "fabric", "quilt", "forge", "neoforge"].contains(&loader.as_str())
    {
        return Err("Некорректные параметры профиля".into());
    }
    let c = db(&app)?;
    let id = Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO profiles(id,name,version,loader) VALUES(?1,?2,?3,?4)",
        params![id, name, version, loader],
    )
    .map_err(err)?;
    set(&c, "active_profile", &id)
}
#[tauri::command]
fn delete_profile(app: tauri::AppHandle, tasks: State<Tasks>, id: String) -> Result<()> {
    let guard = tasks.0.lock().map_err(err)?;
    if guard.contains(&id) {
        return Err("Профиль занят: дождитесь завершения игры или установки".into());
    }
    let c = db(&app)?;
    profile(&c, &id)?;
    let dir = profile_dir(&app, &id)?;
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(err)?;
    }
    c.execute("DELETE FROM profiles WHERE id=?1", [&id])
        .map_err(err)?;
    if get(&c, "active_profile")?.as_deref() == Some(&id) {
        c.execute("DELETE FROM settings WHERE key='active_profile'", [])
            .map_err(err)?;
    }
    Ok(())
}
fn copy_instance(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    fs::create_dir(dst).map_err(err)?;
    for entry in fs::read_dir(src).map_err(err)? {
        let entry = entry.map_err(err)?;
        let kind = entry.file_type().map_err(err)?;
        let target = dst.join(entry.file_name());
        if kind.is_dir() {
            copy_instance(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target).map_err(err)?;
        } else {
            return Err(
                "Сборка содержит ссылку или специальный файл; копирование остановлено".into(),
            );
        }
    }
    Ok(())
}
#[tauri::command]
async fn duplicate_profile(app: tauri::AppHandle, id: String, name: String) -> Result<()> {
    if !valid_name(&name) {
        return Err("Некорректное название сборки".into());
    }
    let busy = acquire(&app, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = busy;
        let c = db(&app)?;
        let p = profile(&c, &id)?;
        let src = profile_dir(&app, &id)?;
        let new_id = Uuid::new_v4().to_string();
        let dest = profile_dir(&app, &new_id)?;
        let result = (|| -> Result<()> {
            if src.exists() {
                fs::create_dir_all(dest.parent().ok_or("Некорректный путь")?).map_err(err)?;
                copy_instance(&src, &dest)?;
            }
            c.execute(
                "INSERT INTO profiles(id,name,version,loader,installed_id) VALUES(?1,?2,?3,?4,?5)",
                params![new_id, name, p.version, p.loader, p.installed_id],
            )
            .map_err(err)?;
            Ok(())
        })();
        if result.is_err() && dest.exists() {
            let _ = fs::remove_dir_all(dest);
        }
        result
    })
    .await
    .map_err(err)?
}
#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: Settings) -> Result<()> {
    if !(512..=65536).contains(&settings.memory_mb)
        || settings.java.trim().is_empty()
        || settings.java.contains('\0')
    {
        return Err("Укажите auto или путь к Java и память от 512 до 65536 МБ".into());
    }
    let c = db(&app)?;
    set(&c, "java", settings.java.trim())?;
    set(&c, "memory_mb", &settings.memory_mb.to_string())
}
fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent("Horizon/0.1.0 (Minecraft launcher)")
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .map_err(err)
}
#[tauri::command]
async fn versions() -> Result<serde_json::Value> {
    tauri::async_runtime::spawn_blocking(|| {
        let v: serde_json::Value = client()?
            .get("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .json()
            .map_err(err)?;
        Ok(v["versions"].clone())
    })
    .await
    .map_err(err)?
}
struct Busy {
    app: tauri::AppHandle,
    id: String,
}
impl Drop for Busy {
    fn drop(&mut self) {
        if let Ok(mut g) = self.app.state::<Tasks>().0.lock() {
            g.remove(&self.id);
        }
    }
}
fn acquire(app: &tauri::AppHandle, id: &str) -> Result<Busy> {
    let state = app.state::<Tasks>();
    let mut g = state.0.lock().map_err(err)?;
    if !g.insert(id.to_owned()) {
        return Err("Профиль уже используется".into());
    }
    Ok(Busy {
        app: app.clone(),
        id: id.to_owned(),
    })
}
#[tauri::command]
async fn install_profile(app: tauri::AppHandle, id: String) -> Result<()> {
    let busy = acquire(&app, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = busy;
        let c = db(&app)?;
        let p = profile(&c, &id)?;
        let dir = profile_dir(&app, &id)?;
        fs::create_dir_all(&dir).map_err(err)?;
        let metadata =
            mc_launcher_core::install::client::fetch_vanilla_version(&p.version).map_err(err)?;
        let major = metadata
            .java_version
            .as_ref()
            .map(|v| v.major_version)
            .unwrap_or(8);
        let major = u32::try_from(major).map_err(err)?;
        let java = {
            let state = app.state::<RuntimeLock>();
            let _lock = state.0.lock().map_err(err)?;
            java::resolve(
                &root(&app)?,
                &get(&c, "java")?.unwrap_or_else(|| "auto".into()),
                major,
                |s| {
                    let _ = app.emit("install-progress", s.to_string());
                },
            )?
        };
        let loader = match p.loader.as_str() {
            "fabric" => Some(LoaderSpec::Fabric {
                version: LoaderVersion::LatestStable,
            }),
            "quilt" => Some(LoaderSpec::Quilt {
                version: LoaderVersion::LatestStable,
            }),
            "forge" => Some(LoaderSpec::Forge {
                version: LoaderVersion::LatestStable,
            }),
            "neoforge" => Some(LoaderSpec::NeoForge {
                version: LoaderVersion::LatestStable,
            }),
            _ => None,
        };
        // A failed reinstallation must not leave a profile marked as ready.
        c.execute("UPDATE profiles SET installed_id=NULL WHERE id=?1", [&id])
            .map_err(err)?;
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let mut progress = |ev: ProgressEvent| {
            if last.elapsed() >= std::time::Duration::from_millis(250) {
                match ev {
                    ProgressEvent::TaskStarted { label, .. }
                    | ProgressEvent::TaskFinished { label } => {
                        let _ = app.emit("install-progress", label);
                        last = std::time::Instant::now();
                    }
                    _ => {}
                }
            }
        };
        let result = Launcher::new(dir)
            .install_with_progress_and_java(
                InstallRequest {
                    minecraft_version: p.version,
                    loader,
                    java: JavaInstallPolicy::Never,
                },
                &mut progress,
                &java,
            )
            .map_err(err)?;
        c.execute(
            "UPDATE profiles SET installed_id=?1 WHERE id=?2",
            params![result.version_id, id],
        )
        .map_err(err)?;
        Ok(())
    })
    .await
    .map_err(err)?
}
#[tauri::command]
async fn launch_profile(app: tauri::AppHandle, id: String) -> Result<()> {
    let busy = acquire(&app, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let c = db(&app)?;
        let p = profile(&c, &id)?;
        let vid = p.installed_id.ok_or("Сначала установите профиль")?;
        let aid = get(&c, "active_account")?.ok_or("Выберите офлайн-аккаунт")?;
        let name: String = c
            .query_row("SELECT name FROM accounts WHERE id=?1", [aid], |r| r.get(0))
            .map_err(err)?;
        let dir = profile_dir(&app, &id)?;
        let game = dir.join("game");
        fs::create_dir_all(&game).map_err(err)?;
        let launcher = Launcher::new(&dir);
        let version = launcher.load_version(&vid).map_err(err)?;
        let base = launcher.load_version(&p.version).map_err(err)?;
        let major = u32::try_from(
            base.java_version
                .as_ref()
                .map(|v| v.major_version)
                .unwrap_or(8),
        )
        .map_err(err)?;
        let java = {
            let state = app.state::<RuntimeLock>();
            let _lock = state.0.lock().map_err(err)?;
            java::resolve(
                &root(&app)?,
                &get(&c, "java")?.unwrap_or_else(|| "auto".into()),
                major,
                |s| {
                    let _ = app.emit("install-progress", s.to_string());
                },
            )?
        };
        let account = Account::Offline {
            username: name.clone(),
            uuid: offline_uuid(&name),
        };
        let options = LaunchOptions {
            account,
            java_executable: Some(java),
            game_directory: Some(game.clone()),
            launcher_name: "Horizon".into(),
            launcher_version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        };
        let cmd = launcher
            .build_launch_command_from_version(&version, options)
            .map_err(err)?;
        let mem = get(&c, "memory_mb")?
            .and_then(|x| x.parse::<u32>().ok())
            .unwrap_or(2048);
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(game.join("horizon-game.log"))
            .map_err(err)?;
        let log2 = log.try_clone().map_err(err)?;
        let mut child = Command::new(cmd.executable)
            .arg(format!("-Xmx{mem}M"))
            .args(cmd.args)
            .envs(cmd.env)
            .current_dir(cmd.working_dir)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log2))
            .spawn()
            .map_err(err)?;
        let a = app.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = a.emit(
                "game-exit",
                match status {
                    Ok(s) => format!("Игра завершена: {s}"),
                    Err(e) => format!("Ошибка игры: {e}"),
                },
            );
            drop(busy);
        });
        Ok(())
    })
    .await
    .map_err(err)?
}
#[tauri::command]
fn game_log(app: tauri::AppHandle, id: String) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let c = db(&app)?;
    profile(&c, &id)?;
    let path = profile_dir(&app, &id)?
        .join("game")
        .join("horizon-game.log");
    let mut f = fs::File::open(path).map_err(err)?;
    let size = f.metadata().map_err(err)?.len();
    f.seek(SeekFrom::Start(size.saturating_sub(65536)))
        .map_err(err)?;
    let mut data = Vec::new();
    f.read_to_end(&mut data).map_err(err)?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}
#[derive(Serialize)]
struct ModFile {
    name: String,
    enabled: bool,
}
fn mods_dir(app: &tauri::AppHandle, id: &str) -> Result<PathBuf> {
    let c = db(app)?;
    profile(&c, id)?;
    let p = profile_dir(app, id)?.join("game").join("mods");
    fs::create_dir_all(&p).map_err(err)?;
    Ok(p)
}
fn safe_mod_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
        && (name.ends_with(".jar") || name.ends_with(".jar.disabled"))
}
#[tauri::command]
fn list_mods(app: tauri::AppHandle, id: String) -> Result<Vec<ModFile>> {
    let mut mods = Vec::new();
    for entry in fs::read_dir(mods_dir(&app, &id)?).map_err(err)? {
        let e = entry.map_err(err)?;
        let name = e.file_name().to_string_lossy().to_string();
        if e.file_type().map_err(err)?.is_file() && safe_mod_name(&name) {
            let enabled = name.ends_with(".jar");
            mods.push(ModFile { name, enabled });
        }
    }
    mods.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(mods)
}
#[tauri::command]
fn change_mod(app: tauri::AppHandle, id: String, name: String, action: String) -> Result<()> {
    let _busy = acquire(&app, &id)?;
    if !safe_mod_name(&name) {
        return Err("Недопустимое имя файла".into());
    }
    let dir = mods_dir(&app, &id)?;
    let path = dir.join(&name);
    if !fs::symlink_metadata(&path)
        .map_err(err)?
        .file_type()
        .is_file()
    {
        return Err("Это не обычный файл".into());
    }
    match action.as_str() {
        "delete" => fs::remove_file(path).map_err(err)?,
        "toggle" => {
            let target = if name.ends_with(".disabled") {
                name.trim_end_matches(".disabled").to_string()
            } else {
                format!("{name}.disabled")
            };
            let dest = dir.join(target);
            if dest.exists() {
                return Err("Файл с таким именем уже существует".into());
            }
            fs::rename(path, dest).map_err(err)?;
        }
        _ => return Err("Неизвестное действие".into()),
    };
    Ok(())
}
#[tauri::command]
async fn search_mods(query: String) -> Result<serde_json::Value> {
    if query.len() > 100 {
        return Err("Слишком длинный запрос".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let v: serde_json::Value = client()?
            .get("https://api.modrinth.com/v2/search")
            .query(&[
                ("query", query.as_str()),
                ("limit", "20"),
                ("facets", r#"[["project_type:mod"]]"#),
            ])
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .json()
            .map_err(err)?;
        Ok(v["hits"].clone())
    })
    .await
    .map_err(err)?
}
#[tauri::command]
async fn install_mod(app: tauri::AppHandle, id: String, project: String) -> Result<()> {
    let busy = acquire(&app, &id)?;
    if project.is_empty()
        || project.len() > 64
        || !project
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Некорректный идентификатор проекта".into());
    }
    tauri::async_runtime::spawn_blocking(move || { use std::io::Read; use sha2::{Digest,Sha512}; let _busy=busy; let c=db(&app)?; let p=profile(&c,&id)?; if p.installed_id.is_none() || p.loader=="vanilla" {return Err("Сначала установите сборку с загрузчиком модов".into());} let game_versions=serde_json::to_string(&vec![p.version]).map_err(err)?; let loaders=serde_json::to_string(&vec![p.loader]).map_err(err)?; let url=format!("https://api.modrinth.com/v2/project/{project}/version"); let versions:serde_json::Value=client()?.get(url).query(&[("game_versions",game_versions),("loaders",loaders)]).send().map_err(err)?.error_for_status().map_err(err)?.json().map_err(err)?; let v=versions.as_array().and_then(|a|a.first()).ok_or("Совместимая версия мода не найдена")?; if v["dependencies"].as_array().map_or(false, |a|a.iter().any(|d|d["dependency_type"]=="required")) {return Err("У мода есть обязательные зависимости: их автоматическая установка пока не поддерживается".into());} let files=v["files"].as_array().ok_or("Нет файлов мода")?; let file=files.iter().find(|f|f["primary"]==true).or_else(||files.first()).ok_or("Нет файлов мода")?; let name=file["filename"].as_str().ok_or("Нет имени файла")?; if !safe_mod_name(name) || !name.ends_with(".jar") {return Err("Недопустимое имя файла мода".into());} let url=file["url"].as_str().ok_or("Нет ссылки на файл")?; if !url.starts_with("https://cdn.modrinth.com/") {return Err("Недоверенный источник файла".into());} let hash=file["hashes"]["sha512"].as_str().ok_or("Нет SHA-512 файла")?; let mut resp=client()?.get(url).send().map_err(err)?.error_for_status().map_err(err)?; if resp.content_length().unwrap_or(0)>134_217_728 {return Err("Файл слишком большой".into());} let mut bytes=Vec::new(); resp.by_ref().take(134_217_729).read_to_end(&mut bytes).map_err(err)?; if bytes.len()>134_217_728 || format!("{:x}",Sha512::digest(&bytes))!=hash {return Err("Размер или контрольная сумма файла неверны".into());} let dir=mods_dir(&app,&id)?; let target=dir.join(name); if target.exists() {return Err("Мод с таким именем уже установлен".into());} let temp=dir.join(format!(".{}.tmp",Uuid::new_v4())); fs::write(&temp,bytes).map_err(err)?; if let Err(e)=fs::hard_link(&temp,&target) {let _=fs::remove_file(&temp);return Err(err(e));} fs::remove_file(temp).map_err(err)?; Ok(()) }).await.map_err(err)?
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("Horizon {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    tauri::Builder::default()
        .manage(Tasks::default())
        .manage(RuntimeLock::default())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            save_account,
            delete_account,
            select_item,
            create_profile,
            delete_profile,
            duplicate_profile,
            save_settings,
            versions,
            install_profile,
            launch_profile,
            game_log,
            list_mods,
            change_mod,
            search_mods,
            install_mod
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Horizon");
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_validated() {
        assert_eq!(
            offline_uuid("Notch"),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
        assert!(valid_username("Steve_123"));
        assert!(!valid_username("../Steve"));
        assert!(!valid_username(""));
        assert!(!valid_username("a very long username"));
        assert!(valid_name("Мой профиль"));
        assert!(!valid_name("\n"));
    }

    #[test]
    fn mod_filenames_cannot_escape_their_directory() {
        assert!(safe_mod_name("example.jar"));
        assert!(safe_mod_name("example.jar.disabled"));
        assert!(!safe_mod_name("../example.jar"));
        assert!(!safe_mod_name("a\\example.jar"));
        assert!(!safe_mod_name(".example.jar"));
        assert!(!safe_mod_name("example.zip"));
    }
    #[test]
    fn instance_copy_is_independent() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("from");
        let to = temp.path().join("to");
        fs::create_dir(&from).unwrap();
        fs::write(from.join("world"), b"original").unwrap();
        copy_instance(&from, &to).unwrap();
        fs::write(to.join("world"), b"changed").unwrap();
        assert_eq!(fs::read(from.join("world")).unwrap(), b"original");
    }

    #[test]
    fn verified_download_is_reused_and_failed_download_preserves_old_file() {
        use mc_launcher_core::net::download::{execute_plan, Checksum, DownloadPlan, DownloadTask};
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("file");
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            for body in ["hello", "wrong", "wrong", "wrong"] {
                let (mut stream, _) = server.accept().unwrap();
                let mut buf = [0; 4096];
                let _ = stream.read(&mut buf);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n{body}"
                )
                .unwrap();
            }
        });
        let task = DownloadTask {
            url: format!("http://{addr}/file"),
            destination: target.clone(),
            checksum: Some(Checksum::Sha1(
                "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".into(),
            )),
            label: "test".into(),
        };
        let plan = DownloadPlan {
            tasks: vec![task.clone()],
        };
        let mut ignored = |_ev: ProgressEvent| {};
        execute_plan(&plan, &mut ignored).unwrap();
        let mut skipped = false;
        execute_plan(&plan, &mut |ev| {
            if matches!(ev, ProgressEvent::TaskSkipped { .. }) {
                skipped = true;
            }
        })
        .unwrap();
        assert!(skipped);
        let mut wrong_plan = plan.clone();
        wrong_plan.tasks[0].checksum = Some(Checksum::Sha1(
            "0000000000000000000000000000000000000000".into(),
        ));
        assert!(execute_plan(&wrong_plan, &mut ignored).is_err());
        assert_eq!(fs::read(target).unwrap(), b"hello");
        worker.join().unwrap();
    }

    /// Live smoke test: deliberately ignored by `cargo test` because it downloads
    /// Minecraft and Java and requires a working X display and OpenGL.
    #[test]
    #[ignore]
    fn live_minecraft_vanilla_and_fabric() {
        use std::time::{Duration, Instant};
        let temp = tempfile::tempdir().unwrap();
        let java = java::resolve(temp.path(), "auto", 17, |s| eprintln!("{s}")).unwrap();
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
            let game = temp.path().join(format!("game-{number}"));
            fs::create_dir_all(&game).unwrap();
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
                if child.try_wait().unwrap().is_some() || start.elapsed() > Duration::from_secs(180)
                {
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
}
