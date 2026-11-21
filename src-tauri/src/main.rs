#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod catalog;
mod integrity;
mod java;
mod modrinth;
#[cfg(any(test, feature = "smoke-test"))]
mod smoke;
use mc_launcher_core::prelude::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
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
#[derive(Default)]
struct Installations(Mutex<HashMap<String, Arc<AtomicBool>>>);
struct InstallationGuard {
    app: tauri::AppHandle,
    id: String,
}
impl Drop for InstallationGuard {
    fn drop(&mut self) {
        if let Ok(mut m) = self.app.state::<Installations>().0.lock() {
            m.remove(&self.id);
        }
    }
}
struct InstallProgress {
    app: tauri::AppHandle,
    id: String,
    cancel: Arc<AtomicBool>,
    last: std::time::Instant,
    completed: usize,
    reused: usize,
    total: usize,
    received: u64,
    per_file: HashMap<String, u64>,
    batch_started: std::time::Instant,
    cache: PathBuf,
}
impl mc_launcher_core::progress::ProgressReporter for InstallProgress {
    fn report(&mut self, ev: ProgressEvent) {
        let label = match ev {
            ProgressEvent::BatchStarted { total } => {
                self.completed = 0;
                self.reused = 0;
                self.total = total;
                self.received = 0;
                self.per_file.clear();
                self.batch_started = std::time::Instant::now();
                format!("Новый этап: файлов {total}")
            }
            ProgressEvent::TaskStarted { label, .. } => label,
            ProgressEvent::TaskFinished { label } => {
                self.completed += 1;
                label
            }
            ProgressEvent::TaskSkipped { label, .. } => {
                self.reused += 1;
                label
            }
            ProgressEvent::BytesReceived {
                label,
                received,
                total,
            } => {
                let previous = self.per_file.insert(label.clone(), received).unwrap_or(0);
                self.received += if received >= previous {
                    received - previous
                } else {
                    received
                };
                match total.filter(|n| *n > 0) {
                    Some(total) => {
                        format!("{} · файл {}%", label, received.saturating_mul(100) / total)
                    }
                    None => format!("{} · {} КБ", label, received / 1024),
                }
            }
            _ => return,
        };
        if self.last.elapsed() >= std::time::Duration::from_millis(250) {
            let done = self.completed + self.reused;
            let speed = self.received as f64
                / self.batch_started.elapsed().as_secs_f64().max(0.01)
                / 1_000_000.0;
            emit_install(&self.app, &self.id, &format!("{label} · этап {done}/{} · осталось {} · загружено {} · из кэша {} · {:.2} МБ/с",self.total,self.total.saturating_sub(done),self.completed,self.reused,speed));
            self.last = std::time::Instant::now();
        }
    }
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn cache_dir(&self) -> Option<PathBuf> {
        Some(self.cache.clone())
    }
}
fn emit_install(app: &tauri::AppHandle, id: &str, message: &str) {
    let _ = app.emit(
        "profile-progress",
        serde_json::json!({"id":id,"message":message}),
    );
}
#[tauri::command]
fn cancel_install(app: tauri::AppHandle, id: String) -> Result<()> {
    let state = app.state::<Installations>();
    let guard = state.0.lock().map_err(err)?;
    let token = guard.get(&id).ok_or("Установка уже завершена")?;
    token.store(true, Ordering::Relaxed);
    emit_install(&app, &id, "Ожидание завершения текущей операции…");
    Ok(())
}
#[derive(Serialize)]
struct ProfileStatus {
    state: &'static str,
    reason: Option<String>,
    path: String,
}
fn actual_status(dir: &std::path::Path, installed: Option<&str>) -> ProfileStatus {
    let path = dir.join("game").to_string_lossy().to_string();
    match installed {
        None => ProfileStatus {
            state: if dir.join(".horizon-installing").exists() || dir.join("versions").exists() {
                "broken"
            } else {
                "not_installed"
            },
            reason: None,
            path,
        },
        Some(id) => match integrity::verify(dir, id) {
            Ok(()) => ProfileStatus {
                state: "ready",
                reason: None,
                path,
            },
            Err(e) => ProfileStatus {
                state: "broken",
                reason: Some(e),
                path,
            },
        },
    }
}
#[tauri::command]
async fn profile_status(app: tauri::AppHandle, id: String) -> Result<ProfileStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = acquire(&app, &id)?;
        let c = db(&app)?;
        let p = profile(&c, &id)?;
        let dir = profile_dir(&app, &id)?;
        Ok(actual_status(&dir, p.installed_id.as_deref()))
    })
    .await
    .map_err(err)?
}
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
    java: Option<String>,
    memory_mb: Option<u32>,
    last_played: Option<String>,
    mod_count: usize,
    jvm_args: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
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
    c.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(err)?;
    init_db(&c)?;
    Ok(c)
}
fn init_db(c: &Connection) -> Result<()> {
    c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS accounts(id TEXT PRIMARY KEY,name TEXT NOT NULL); CREATE TABLE IF NOT EXISTS profiles(id TEXT PRIMARY KEY,name TEXT NOT NULL,version TEXT NOT NULL,loader TEXT NOT NULL,installed_id TEXT); CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);").map_err(err)?;
    for (column, declaration) in [
        ("java", "TEXT"),
        ("memory_mb", "INTEGER"),
        ("last_played", "TEXT"),
        ("jvm_args", "TEXT"),
        ("width", "INTEGER"),
        ("height", "INTEGER"),
    ] {
        let has_column = || -> Result<bool> {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('profiles') WHERE name=?1)",
                [column],
                |r| r.get(0),
            )
            .map_err(err)
        };
        if !has_column()? {
            if let Err(e) = c.execute_batch(&format!(
                "ALTER TABLE profiles ADD COLUMN {column} {declaration}"
            )) {
                // Another connection may have migrated the schema in the meantime.
                if !has_column()? {
                    return Err(err(e));
                }
            }
        }
    }
    Ok(())
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
        "SELECT id,name,version,loader,installed_id,java,memory_mb,last_played,jvm_args,width,height FROM profiles WHERE id=?1",
        [id],
        |r| {
            Ok(Profile {
                id: r.get(0)?,
                name: r.get(1)?,
                version: r.get(2)?,
                loader: r.get(3)?,
                installed_id: r.get(4)?,
                java: r.get(5)?,
                memory_mb: r.get(6)?,
                last_played: r.get(7)?,
                mod_count: 0,
                jvm_args: r.get(8)?,
                width: r.get(9)?,
                height: r.get(10)?,
            })
        },
    )
    .map_err(err)
}
// Reject symlinked managed directories instead of following them outside the app data tree.
fn managed_child(parent: &std::path::Path, name: &str, create: bool) -> Result<PathBuf> {
    let path = parent.join(name);
    if create {
        match fs::create_dir(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(err(e)),
        }
    }
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_dir() => (),
        Ok(_) => return Err("Каталог сборки является ссылкой или не является каталогом".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => (),
        Err(e) => return Err(err(e)),
    }
    Ok(path)
}
fn profile_dir(app: &tauri::AppHandle, id: &str) -> Result<PathBuf> {
    Uuid::parse_str(id).map_err(err)?;
    let instances = managed_child(&root(app)?, "instances", true)?;
    managed_child(&instances, id, false)
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
    let mut profiles = c
        .prepare("SELECT id,name,version,loader,installed_id,java,memory_mb,last_played,jvm_args,width,height FROM profiles ORDER BY rowid")
        .map_err(err)?
        .query_map([], |r| {
            Ok(Profile {
                id: r.get(0)?,
                name: r.get(1)?,
                version: r.get(2)?,
                loader: r.get(3)?,
                installed_id: r.get(4)?,
                java: r.get(5)?,
                memory_mb: r.get(6)?,
                last_played: r.get(7)?,
                mod_count: 0,
                jvm_args: r.get(8)?,
                width: r.get(9)?,
                height: r.get(10)?,
            })
        })
        .map_err(err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(err)?;
    for p in &mut profiles {
        // Only count ordinary .jar files in this profile; missing folders are normal.
        if let Ok(dir) = profile_dir(&app, &p.id) {
            if let Ok(game) = managed_child(&dir, "game", false) {
                if let Ok(mods) = managed_child(&game, "mods", false) {
                    if let Ok(entries) = fs::read_dir(mods) {
                        p.mod_count = entries
                            .flatten()
                            .filter(|e| {
                                e.file_type().map(|t| t.is_file()).unwrap_or(false)
                                    && safe_mod_name(&e.file_name().to_string_lossy())
                            })
                            .count();
                    }
                }
            }
        }
    }
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
fn rename_profile(app: tauri::AppHandle, id: String, name: String) -> Result<()> {
    if !valid_name(&name) {
        return Err("Некорректное название сборки".into());
    }
    let c = db(&app)?;
    if c.execute(
        "UPDATE profiles SET name=?1 WHERE id=?2",
        params![name.trim(), id],
    )
    .map_err(err)?
        == 0
    {
        return Err("Сборка не найдена".into());
    }
    Ok(())
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
                "INSERT INTO profiles(id,name,version,loader,installed_id,java,memory_mb,jvm_args,width,height) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![new_id, name, p.version, p.loader, p.installed_id, p.java, p.memory_mb, p.jvm_args, p.width, p.height],
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
// One argument per line; passed directly to Java, never via a shell.
fn parse_jvm_args(text: &str) -> Result<Vec<String>> {
    if text.len() > 4096 {
        return Err("Слишком много аргументов JVM".into());
    }
    let mut result = Vec::new();
    for line in text.lines() {
        let arg = line.trim();
        if arg.is_empty() {
            continue;
        }
        if result.len() >= 64
            || !arg.starts_with('-')
            || arg.chars().any(char::is_control)
            || arg.starts_with("-Xmx")
            || arg == "-cp"
            || arg == "-classpath"
            || arg == "--class-path"
            || arg == "-jar"
        {
            return Err("Каждый аргумент JVM должен быть отдельной строкой; память и classpath настраиваются лаунчером".into());
        }
        result.push(arg.to_string());
    }
    Ok(result)
}
#[tauri::command]
fn save_profile_settings(
    app: tauri::AppHandle,
    id: String,
    java: Option<String>,
    memory_mb: Option<u32>,
    jvm_args: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<()> {
    let _busy = acquire(&app, &id)?;
    if memory_mb.is_some_and(|m| !(512..=65536).contains(&m))
        || java
            .as_ref()
            .is_some_and(|j| j.trim().is_empty() || j.contains('\0'))
    {
        return Err("Укажите auto или путь к Java и память от 512 до 65536 МБ".into());
    }
    if width.is_some() != height.is_some()
        || width.is_some_and(|x| !(320..=7680).contains(&x))
        || height.is_some_and(|x| !(240..=4320).contains(&x))
    {
        return Err("Укажите ширину и высоту окна в допустимых пределах".into());
    }
    if let Some(ref args) = jvm_args {
        parse_jvm_args(args)?;
    }
    let c = db(&app)?;
    profile(&c, &id)?;
    c.execute(
        "UPDATE profiles SET java=?1, memory_mb=?2, jvm_args=?3, width=?4, height=?5 WHERE id=?6",
        params![
            java.map(|j| j.trim().to_string()),
            memory_mb,
            jvm_args,
            width,
            height,
            id
        ],
    )
    .map_err(err)?;
    Ok(())
}
#[tauri::command]
async fn open_profile_folder(app: tauri::AppHandle, id: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let c = db(&app)?;
        profile(&c, &id)?;
        let dir = profile_dir(&app, &id)?;
        let dir = managed_child(dir.parent().ok_or("Некорректный каталог")?, &id, true)?;
        let game = managed_child(&dir, "game", true)?;
        let status = Command::new("xdg-open").arg(game).status().map_err(err)?;
        if !status.success() {
            return Err(format!("Не удалось открыть папку: {status}"));
        }
        Ok(())
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
        .user_agent(
            "BalaRmack/Horizon/0.1.0 (https://github.com/BalaRmack/minecraft-launcher-linux)",
        )
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
    let cancel = Arc::new(AtomicBool::new(false));
    app.state::<Installations>()
        .0
        .lock()
        .map_err(err)?
        .insert(id.clone(), cancel.clone());
    let installation = InstallationGuard {
        app: app.clone(),
        id: id.clone(),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = busy;
        let _installation = installation;
        let c = db(&app)?;
        let p = profile(&c, &id)?;
        let dir = profile_dir(&app, &id)?;
        let dir = managed_child(
            dir.parent().ok_or("Некорректный каталог сборки")?,
            &id,
            true,
        )?;
        fs::write(dir.join(".horizon-installing"), b"installation incomplete").map_err(err)?;
        // Clear before any network or Java work so an interrupted repair is never ready.
        c.execute("UPDATE profiles SET installed_id=NULL WHERE id=?1", [&id])
            .map_err(err)?;
        emit_install(&app, &id, "Получение метаданных Minecraft…");
        let metadata =
            mc_launcher_core::install::client::fetch_vanilla_version(&p.version).map_err(err)?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Установка отменена".into());
        }
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
                &p.java
                    .clone()
                    .or(get(&c, "java")?)
                    .unwrap_or_else(|| "auto".into()),
                major,
                |s| {
                    emit_install(&app, &id, s);
                },
            )?
        };
        if cancel.load(Ordering::Relaxed) {
            return Err("Установка отменена".into());
        }
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
        let mut progress = InstallProgress {
            app: app.clone(),
            id: id.clone(),
            cancel: cancel.clone(),
            last: std::time::Instant::now() - std::time::Duration::from_secs(1),
            completed: 0,
            reused: 0,
            total: 0,
            received: 0,
            per_file: HashMap::new(),
            batch_started: std::time::Instant::now(),
            cache: managed_child(&root(&app)?, "downloads-cache", true)?,
        };
        let result = Launcher::new(&dir)
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
        if cancel.load(Ordering::Relaxed) {
            return Err("Установка отменена".into());
        }
        emit_install(&app, &id, "Проверка установленных файлов…");
        integrity::verify(&dir, &result.version_id)?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Установка отменена".into());
        }
        c.execute(
            "UPDATE profiles SET installed_id=?1 WHERE id=?2",
            params![result.version_id, id],
        )
        .map_err(err)?;
        let _ = fs::remove_file(dir.join(".horizon-installing"));
        Ok(())
    })
    .await
    .map_err(err)?
}
// No credentials, session cookies, full environment or account details are written.
fn graphics_diagnostics() -> String {
    use std::fmt::Write as _;
    let mut out = String::from("\n[Horizon graphics diagnostics]\n");
    for key in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "GDK_BACKEND",
        "LIBGL_ALWAYS_SOFTWARE",
    ] {
        let value = std::env::var(key).unwrap_or_else(|_| "<unset>".into());
        let clean: String = value
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        let _ = writeln!(out, "{key}={clean}");
    }
    let _ = writeln!(
        out,
        "XAUTHORITY={} (contents not logged)",
        if std::env::var_os("XAUTHORITY").is_some() {
            "set"
        } else {
            "unset"
        }
    );
    let _ = writeln!(
        out,
        "X11 socket directory present={}; DRI directory present={}",
        std::path::Path::new("/tmp/.X11-unix").is_dir(),
        std::path::Path::new("/dev/dri").is_dir()
    );
    if let Ok(info) = fs::read_to_string("/.flatpak-info") {
        let mut context = false;
        for line in info.lines() {
            if line.starts_with('[') {
                context = line == "[Context]";
                continue;
            }
            if context
                && (line.starts_with("shared=")
                    || line.starts_with("sockets=")
                    || line.starts_with("devices="))
            {
                let _ = writeln!(out, "Flatpak {line}");
            }
        }
    } else {
        let _ = writeln!(out, "Flatpak sandbox: no /.flatpak-info");
    }
    out.push_str("Java inherits the launcher's environment; no display variables are cleared.\n");
    out
}
#[tauri::command]
async fn launch_profile(app: tauri::AppHandle, id: String) -> Result<()> {
    let busy = acquire(&app, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let c = db(&app)?;
        let p = profile(&c, &id)?;
        let vid = p.installed_id.clone().ok_or("Сначала установите профиль")?;
        let dir = profile_dir(&app, &id)?;
        integrity::verify(&dir, &vid).map_err(|e| format!("Сборку необходимо восстановить: {e}"))?;
        let aid = get(&c, "active_account")?.ok_or("Выберите офлайн-аккаунт")?;
        let name: String = c
            .query_row("SELECT name FROM accounts WHERE id=?1", [aid], |r| r.get(0))
            .map_err(err)?;
        let game = managed_child(&dir, "game", true)?;
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
                &p.java
                    .clone()
                    .or(get(&c, "java")?)
                    .unwrap_or_else(|| "auto".into()),
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
            custom_resolution: p.width.zip(p.height),
            ..Default::default()
        };
        let cmd = launcher
            .build_launch_command_from_version(&version, options)
            .map_err(err)?;
        let mem = p
            .memory_mb
            .or(get(&c, "memory_mb")?.and_then(|x| x.parse::<u32>().ok()))
            .unwrap_or(2048);
        let mut log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(game.join("horizon-game.log"))
            .map_err(err)?;
        use std::io::Write as _;
        log.write_all(graphics_diagnostics().as_bytes()).map_err(err)?;
        let log2 = log.try_clone().map_err(err)?;
        let mut child = Command::new(cmd.executable)
            .arg(format!("-Xmx{mem}M"))
            .args(parse_jvm_args(p.jvm_args.as_deref().unwrap_or(""))?)
            .args(cmd.args)
            .envs(cmd.env)
            .current_dir(cmd.working_dir)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log2))
            .spawn()
            .map_err(err)?;
        // Record only after Java actually starts; a failed spawn must not change the date.
        if let Err(e) = c.execute("UPDATE profiles SET last_played=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id=?1", [&id]) { eprintln!("Could not update last played: {e}"); }
        let a = app.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            let event = match status {
                Ok(s) => serde_json::json!({"profile_id": id, "success": s.success(), "status": s.to_string()}),
                Err(e) => serde_json::json!({"profile_id": id, "success": false, "status": e.to_string()}),
            };
            let _ = a.emit("game-exit", event);
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
    let dir = profile_dir(&app, &id)?;
    let path = managed_child(&dir, "game", false)?.join("horizon-game.log");
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
    let dir = profile_dir(app, id)?;
    let dir = managed_child(dir.parent().ok_or("Некорректный каталог сборки")?, id, true)?;
    let game = managed_child(&dir, "game", true)?;
    managed_child(&game, "mods", true)
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
async fn catalog_search(
    app: tauri::AppHandle,
    params: catalog::Search,
) -> Result<serde_json::Value> {
    tauri::async_runtime::spawn_blocking(move || {
        catalog::search(&app.state::<catalog::Cache>(), params)
    })
    .await
    .map_err(err)?
}
#[tauri::command]
async fn catalog_categories(app: tauri::AppHandle) -> Result<serde_json::Value> {
    tauri::async_runtime::spawn_blocking(move || {
        catalog::categories(&app.state::<catalog::Cache>())
    })
    .await
    .map_err(err)?
}
#[tauri::command]
async fn catalog_details(
    app: tauri::AppHandle,
    project: String,
    version: String,
    loader: String,
) -> Result<serde_json::Value> {
    tauri::async_runtime::spawn_blocking(move || {
        catalog::details(&app.state::<catalog::Cache>(), &project, &version, &loader)
    })
    .await
    .map_err(err)?
}
#[tauri::command]
async fn inspect_mods(app: tauri::AppHandle, id: String) -> Result<serde_json::Value> {
    tauri::async_runtime::spawn_blocking(move || {
        let c = db(&app)?;
        let p = profile(&c, &id)?;
        let dir = mods_dir(&app, &id)?;
        catalog::inspect(&app.state::<catalog::Cache>(), &dir, &p.version, &p.loader)
    })
    .await
    .map_err(err)?
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
async fn mod_details(project: String) -> Result<serde_json::Value> {
    if project.is_empty()
        || project.len() > 64
        || !project
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Некорректный идентификатор проекта".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        client()?
            .get(format!("https://api.modrinth.com/v2/project/{project}"))
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .json()
            .map_err(err)
    })
    .await
    .map_err(err)?
}
#[tauri::command]
async fn install_mod(app: tauri::AppHandle, id: String, project: String) -> Result<usize> {
    let busy = acquire(&app, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = busy;
        let c = db(&app)?;
        let p = profile(&c, &id)?;
        if p.installed_id.is_none() || p.loader == "vanilla" {
            return Err("Сначала установите сборку с загрузчиком модов".into());
        }
        let dir = profile_dir(&app, &id)?;
        integrity::verify(&dir, p.installed_id.as_deref().unwrap())
            .map_err(|e| format!("Сначала восстановите сборку: {e}"))?;
        modrinth::install(
            &mods_dir(&app, &id)?,
            &project,
            &p.version,
            &p.loader,
            |s| {
                let _ = app.emit("install-progress", s.to_string());
            },
        )
    })
    .await
    .map_err(err)?
}
fn main() {
    #[cfg(feature = "smoke-test")]
    if std::env::args().nth(1).as_deref() == Some("--smoke-test") {
        smoke::run();
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("Horizon {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    tauri::Builder::default()
        .manage(Tasks::default())
        .manage(catalog::Cache::default())
        .manage(RuntimeLock::default())
        .manage(Installations::default())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            save_account,
            delete_account,
            select_item,
            create_profile,
            rename_profile,
            delete_profile,
            duplicate_profile,
            save_profile_settings,
            open_profile_folder,
            save_settings,
            versions,
            install_profile,
            cancel_install,
            profile_status,
            launch_profile,
            game_log,
            list_mods,
            change_mod,
            search_mods,
            catalog_search,
            catalog_categories,
            catalog_details,
            inspect_mods,
            mod_details,
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
    fn installation_state_uses_files_and_interrupted_marker() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(actual_status(temp.path(), None).state, "not_installed");
        assert_eq!(actual_status(temp.path(), Some("missing")).state, "broken");
        fs::write(temp.path().join(".horizon-installing"), b"incomplete").unwrap();
        assert_eq!(actual_status(temp.path(), None).state, "broken");
    }
    #[test]
    fn jvm_arguments_are_separate_and_do_not_override_managed_memory() {
        assert_eq!(
            parse_jvm_args("-XX:+UseG1GC\n-Dtest=hello world")
                .unwrap()
                .len(),
            2
        );
        assert!(parse_jvm_args("-Xmx8G").is_err());
        assert!(parse_jvm_args("-cp").is_err());
        assert!(parse_jvm_args("java").is_err());
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
    fn migrates_old_profile_database_without_losing_data() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE profiles(id TEXT PRIMARY KEY,name TEXT NOT NULL,version TEXT NOT NULL,loader TEXT NOT NULL,installed_id TEXT); INSERT INTO profiles VALUES ('a','world','1.20.1','fabric','installed');").unwrap();
        init_db(&c).unwrap();
        init_db(&c).unwrap();
        let p = profile(&c, "a").unwrap();
        assert_eq!(p.name, "world");
        assert_eq!(p.installed_id.as_deref(), Some("installed"));
        assert!(p.java.is_none() && p.memory_mb.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn managed_directories_reject_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/tmp", temp.path().join("escape")).unwrap();
        assert!(managed_child(temp.path(), "escape", true).is_err());
        assert!(managed_child(temp.path(), "escape", false).is_err());
    }

    #[test]
    fn cancelled_download_plan_stops_before_writing() {
        use mc_launcher_core::{
            net::download::{execute_plan, DownloadPlan, DownloadTask},
            progress::ProgressReporter,
        };
        struct Cancelled;
        impl ProgressReporter for Cancelled {
            fn report(&mut self, _event: ProgressEvent) {}
            fn is_cancelled(&self) -> bool {
                true
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("file");
        let plan = DownloadPlan {
            tasks: vec![DownloadTask {
                url: "http://127.0.0.1:1/no".into(),
                destination: target.clone(),
                checksum: None,
                label: "test".into(),
            }],
        };
        assert!(execute_plan(&plan, &mut Cancelled).is_err());
        assert!(!target.exists());
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
    fn downloads_independent_files_concurrently_and_reuses_shared_cache() {
        use mc_launcher_core::{
            net::download::{execute_plan, Checksum, DownloadPlan, DownloadTask},
            progress::ProgressReporter,
        };
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::atomic::{AtomicUsize, Ordering},
            time::Duration,
        };
        struct Reporter {
            cache: PathBuf,
            reused: usize,
        }
        impl ProgressReporter for Reporter {
            fn report(&mut self, event: ProgressEvent) {
                if matches!(event, ProgressEvent::TaskSkipped { .. }) {
                    self.reused += 1;
                }
            }
            fn cache_dir(&self) -> Option<PathBuf> {
                Some(self.cache.clone())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let worker_active = active.clone();
        let worker_peak = peak.clone();
        let worker = std::thread::spawn(move || {
            let mut children = Vec::new();
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let a = worker_active.clone();
                let p = worker_peak.clone();
                children.push(std::thread::spawn(move || {
                    let mut buf = [0; 4096];
                    let _ = stream.read(&mut buf);
                    let n = a.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(200));
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
                    )
                    .unwrap();
                    a.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            for child in children {
                child.join().unwrap();
            }
        });
        let tasks: Vec<_> = (0..4)
            .map(|i| DownloadTask {
                url: format!("http://{addr}/{i}"),
                destination: temp.path().join(format!("one/{i}")),
                checksum: Some(Checksum::Sha1(
                    "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".into(),
                )),
                label: format!("file-{i}"),
            })
            .collect();
        let mut reporter = Reporter {
            cache: temp.path().join("cache"),
            reused: 0,
        };
        execute_plan(
            &DownloadPlan {
                tasks: tasks.clone(),
            },
            &mut reporter,
        )
        .unwrap();
        worker.join().unwrap();
        assert!(peak.load(Ordering::SeqCst) > 1, "requests must overlap");
        let mut other = tasks;
        for (i, task) in other.iter_mut().enumerate() {
            task.destination = temp.path().join(format!("two/{i}"));
        }
        execute_plan(&DownloadPlan { tasks: other }, &mut reporter).unwrap();
        assert_eq!(reporter.reused, 4);
        fs::write(temp.path().join("one/0"), b"modified").unwrap();
        assert_eq!(fs::read(temp.path().join("two/0")).unwrap(), b"hello");
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

    #[test]
    #[ignore]
    fn live_minecraft_vanilla_and_fabric() {
        crate::smoke::run();
    }
}
