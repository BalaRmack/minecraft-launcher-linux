//! Modrinth version and required dependency resolution, with staged verified writes.
use crate::{client, err, safe_mod_name, Result};
use reqwest::blocking::Client;
use serde_json::Value;
use sha2::{Digest, Sha512};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

static MOD_DOWNLOAD_LIMIT: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
const MAX_FILE: u64 = 128 * 1024 * 1024;
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn contains(value: &Value, field: &str, wanted: &str) -> bool {
    value[field]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(wanted)))
}
fn compatible(version: &Value, minecraft: &str, loader: &str) -> bool {
    contains(version, "game_versions", minecraft) && contains(version, "loaders", loader)
}
fn str_field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("В ответе Modrinth отсутствует {key}"))
}
struct Resolver<'a> {
    client: Client,
    api: String,
    minecraft: &'a str,
    loader: &'a str,
    seen: HashMap<String, String>,
    files: Vec<Value>,
    versions: Vec<Value>,
}
impl Resolver<'_> {
    fn version(&self, project: Option<&str>, id: Option<&str>) -> Result<Value> {
        if project.is_some_and(|s| !valid_id(s)) || id.is_some_and(|s| !valid_id(s)) {
            return Err("Недопустимый идентификатор Modrinth".into());
        }
        let value = if let Some(id) = id {
            self.client
                .get(format!("{}/version/{id}", self.api))
                .send()
                .map_err(err)?
                .error_for_status()
                .map_err(err)?
                .json::<Value>()
                .map_err(err)?
        } else {
            let project = project.ok_or("У зависимости нет идентификатора проекта")?;
            let versions: Value = self
                .client
                .get(format!("{}/project/{project}/version", self.api))
                .query(&[
                    (
                        "game_versions",
                        serde_json::to_string(&[self.minecraft]).map_err(err)?,
                    ),
                    (
                        "loaders",
                        serde_json::to_string(&[self.loader]).map_err(err)?,
                    ),
                ])
                .send()
                .map_err(err)?
                .error_for_status()
                .map_err(err)?
                .json()
                .map_err(err)?;
            versions
                .as_array()
                .and_then(|v| v.first())
                .cloned()
                .ok_or_else(|| format!("Совместимая версия {project} не найдена"))?
        };
        if !compatible(&value, self.minecraft, self.loader) {
            return Err("Версия зависимости несовместима с игрой или загрузчиком".into());
        }
        if id.is_some_and(|i| value["id"].as_str() != Some(i)) {
            return Err("Modrinth вернул неверную версию".into());
        }
        if id.is_some() && project.is_some_and(|p| value["project_id"].as_str() != Some(p)) {
            return Err("Версия зависимости относится к другому проекту".into());
        }
        Ok(value)
    }
    fn resolve(&mut self, project: Option<&str>, id: Option<&str>) -> Result<()> {
        if self.seen.len() >= 64 {
            return Err("Слишком много зависимостей мода".into());
        }
        let version = self.version(project, id)?;
        let project_id = str_field(&version, "project_id")?.to_owned();
        let version_id = str_field(&version, "id")?.to_owned();
        if !valid_id(&project_id) || !valid_id(&version_id) {
            return Err("Недопустимый ответ Modrinth".into());
        }
        if let Some(existing) = self.seen.get(&project_id) {
            return if existing == &version_id {
                Ok(())
            } else {
                Err(format!("Конфликт версий зависимости {project_id}"))
            };
        }
        // Insert before descending, so cycles terminate and conflicts remain detectable.
        self.seen.insert(project_id, version_id);
        if let Some(deps) = version["dependencies"].as_array() {
            for dep in deps.iter().filter(|d| d["dependency_type"] == "required") {
                let p = dep["project_id"].as_str();
                let v = dep["version_id"].as_str();
                if p.is_none() && v.is_none() {
                    return Err("Modrinth не указал проект обязательной зависимости".into());
                }
                self.resolve(p, v)?;
            }
        }
        self.versions.push(version.clone());
        let files = version["files"].as_array().ok_or("Нет файлов мода")?;
        self.files.push(
            files
                .iter()
                .find(|f| f["primary"] == true)
                .or_else(|| files.first())
                .cloned()
                .ok_or("Нет файлов мода")?,
        );
        Ok(())
    }
}
pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(err)?;
    let mut hash = Sha512::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(err)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn metadata(file: &Value) -> Result<(&str, &str, &str)> {
    let name = str_field(file, "filename")?;
    let url = str_field(file, "url")?;
    let hash = file["hashes"]["sha512"]
        .as_str()
        .ok_or("Нет SHA-512 файла")?;
    if !safe_mod_name(name)
        || !name.ends_with(".jar")
        || !url.starts_with("https://cdn.modrinth.com/")
        || hash.len() != 128
        || !hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Недопустимые данные файла Modrinth".into());
    }
    Ok((name, url, hash))
}
fn incompatible_with(a: &Value, b: &Value) -> bool {
    a["dependencies"].as_array().is_some_and(|deps| {
        deps.iter().any(|d| {
            if d["dependency_type"] != "incompatible" {
                return false;
            }
            let project = d["project_id"].as_str();
            let version = d["version_id"].as_str();
            (project.is_some() || version.is_some())
                && project.map_or(true, |p| b["project_id"] == p)
                && version.map_or(true, |v| b["id"] == v)
        })
    })
}
fn check_existing(planned: &[Value], existing: &[Value]) -> Result<()> {
    for v in planned {
        for other in existing.iter().chain(planned.iter()) {
            if v["project_id"] == other["project_id"] && v["id"] != other["id"] {
                return Err(format!("Другая версия мода {} уже установлена или требуется зависимостью; удалите конфликтующий файл вручную", v["project_id"].as_str().unwrap_or("?")));
            }
            if incompatible_with(v, other) || incompatible_with(other, v) {
                return Err(format!(
                    "Modrinth сообщает о несовместимости модов {} и {}",
                    v["project_id"].as_str().unwrap_or("?"),
                    other["project_id"].as_str().unwrap_or("?")
                ));
            }
        }
    }
    Ok(())
}
struct ModDownload {
    name: String,
    url: String,
    hash: String,
    temp: PathBuf,
}
async fn download_mod(client: reqwest::Client, item: ModDownload) -> Result<()> {
    let _permit = MOD_DOWNLOAD_LIMIT
        .get_or_init(|| tokio::sync::Semaphore::new(4))
        .acquire()
        .await
        .map_err(err)?;
    let mut last = String::new();
    for attempt in 0..3 {
        let result: Result<()> = async {
            let mut response = client
                .get(&item.url)
                .send()
                .await
                .map_err(err)?
                .error_for_status()
                .map_err(err)?;
            if response.content_length().unwrap_or(0) > MAX_FILE {
                return Err("Файл мода слишком велик".into());
            }
            let mut out = fs::File::create(&item.temp).map_err(err)?;
            let mut digest = Sha512::new();
            let mut size = 0u64;
            while let Some(chunk) = response.chunk().await.map_err(err)? {
                size += chunk.len() as u64;
                if size > MAX_FILE {
                    return Err("Файл мода слишком велик".into());
                }
                digest.update(&chunk);
                out.write_all(&chunk).map_err(err)?;
            }
            if format!("{:x}", digest.finalize()) != item.hash {
                return Err(format!("Контрольная сумма {} не совпадает", item.name));
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e;
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(400 * (attempt + 1))).await;
                }
            }
        }
    }
    Err(last)
}
fn download_mods(files: Vec<ModDownload>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(err)?;
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .user_agent(
                "BalaRmack/Horizon/0.1.0 (https://github.com/BalaRmack/minecraft-launcher-linux)",
            )
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(err)?;
        let mut pending = files.into_iter();
        let mut jobs = tokio::task::JoinSet::new();
        loop {
            while jobs.len() < 4 {
                if let Some(item) = pending.next() {
                    jobs.spawn(download_mod(client.clone(), item));
                } else {
                    break;
                }
            }
            match jobs.join_next().await {
                None => break,
                Some(Ok(Ok(()))) => (),
                Some(Ok(Err(e))) => {
                    jobs.abort_all();
                    while jobs.join_next().await.is_some() {}
                    return Err(e);
                }
                Some(Err(e)) => {
                    jobs.abort_all();
                    while jobs.join_next().await.is_some() {}
                    return Err(err(e));
                }
            }
        }
        Ok(())
    })
}
/// Downloads all required files before promoting any of them. Never overwrites existing mods.
pub fn install(
    dir: &Path,
    project: &str,
    minecraft: &str,
    loader: &str,
    progress: impl Fn(&str),
) -> Result<usize> {
    install_with_cache(dir, project, minecraft, loader, None, progress)
}
pub fn install_with_cache(
    dir: &Path,
    project: &str,
    minecraft: &str,
    loader: &str,
    cache: Option<&Path>,
    progress: impl Fn(&str),
) -> Result<usize> {
    install_inner(
        dir, project, minecraft, loader, cache, false, None, progress,
    )
}
pub fn update_with_cache(
    dir: &Path,
    project: &str,
    minecraft: &str,
    loader: &str,
    cache: Option<&Path>,
    progress: impl Fn(&str),
) -> Result<usize> {
    install_inner(dir, project, minecraft, loader, cache, true, None, progress)
}
#[cfg(any(test, feature = "smoke-test"))]
pub fn install_exact_for_test(
    dir: &Path,
    project: &str,
    version: &str,
    minecraft: &str,
    loader: &str,
) -> Result<usize> {
    install_inner(
        dir,
        project,
        minecraft,
        loader,
        None,
        false,
        Some(version),
        |_| {},
    )
}
fn install_inner(
    dir: &Path,
    project: &str,
    minecraft: &str,
    loader: &str,
    cache: Option<&Path>,
    updating: bool,
    root_version: Option<&str>,
    progress: impl Fn(&str),
) -> Result<usize> {
    let mut resolver = Resolver {
        client: client()?,
        api: "https://api.modrinth.com/v2".into(),
        minecraft,
        loader,
        seen: HashMap::new(),
        files: Vec::new(),
        versions: Vec::new(),
    };
    resolver.resolve(Some(project), root_version)?;
    fs::create_dir_all(dir).map_err(err)?;
    progress("Проверка известных конфликтов модов");
    let (local, complete) = crate::mod_state::scan(dir)?;
    if !complete {
        return Err("Не удалось проверить установленные моды через Modrinth".into());
    }
    let root = resolver.versions.last().ok_or("Нет версии мода")?;
    let target_id = str_field(root, "project_id")?;
    let existing: Vec<_> = local
        .iter()
        .filter(|f| {
            f.version
                .as_ref()
                .is_some_and(|v| v["project_id"] == target_id)
        })
        .collect();
    if existing.len() > 1 {
        return Err(
            "Найдено несколько файлов этого проекта; проверьте их в разделе Мои моды".into(),
        );
    }
    if !updating && !existing.is_empty() {
        return Ok(0);
    }
    if local.iter().any(|file| file.version.is_none()) {
        return Err("Есть неопознанные файлы модов: нельзя исключить дубликаты или конфликты. Проверьте раздел Мои моды".into());
    }
    let old = if updating {
        let old = existing
            .first()
            .ok_or("Установленный мод не найден; обновите каталог")?;
        if !old.name.ends_with(".jar") {
            return Err("Сначала включите мод в разделе Мои моды".into());
        }
        let prior = old.version.as_ref().ok_or("Версия мода неизвестна")?;
        if root["id"] == prior["id"]
            || !root["date_published"]
                .as_str()
                .zip(prior["date_published"].as_str())
                .is_some_and(|(a, b)| a > b)
        {
            return Err("Более новая совместимая версия не найдена".into());
        }
        Some(*old)
    } else {
        None
    };
    let other_versions: Vec<Value> = local
        .iter()
        .filter(|f| old.map_or(true, |o| o.name != f.name))
        .filter(|f| f.name.ends_with(".jar"))
        .filter_map(|f| f.version.clone())
        .collect();
    check_existing(&resolver.versions, &other_versions)?;
    if old.is_some() {
        for other in &other_versions {
            if other["dependencies"].as_array().is_some_and(|deps| {
                deps.iter().any(|dep| {
                    dep["dependency_type"] == "required"
                        && dep["project_id"] == target_id
                        && dep["version_id"]
                            .as_str()
                            .is_some_and(|version| root["id"] != version)
                })
            }) {
                return Err(
                    "Другому установленному моду требуется прежняя версия; обновление остановлено"
                        .into(),
                );
            }
        }
    }
    let mut records = crate::mod_state::ledger(dir)?;
    let stage = tempfile::tempdir_in(dir).map_err(err)?;
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut names = HashMap::new();
    let mut downloads = Vec::new();
    let mut cache_writes = Vec::new();
    for (file, version) in resolver.files.iter().zip(&resolver.versions) {
        let (name, url, hash) = metadata(file)?;
        records.insert(hash.to_ascii_lowercase(), version.clone());
        if let Some(previous) = names.insert(name.to_string(), hash.to_string()) {
            if previous != hash {
                return Err("Два мода имеют одинаковые имена файлов".into());
            }
            continue;
        }
        let dest = dir.join(name);
        if dir.join(format!("{name}.disabled")).exists() {
            return Err(format!(
                "Мод {name} отключён: сначала включите или удалите его"
            ));
        }
        if let Ok(meta) = fs::symlink_metadata(&dest) {
            if old.is_some_and(|o| o.name == name) {
                if !meta.file_type().is_file() || hash_file(&dest)? != old.unwrap().hash {
                    return Err("Файл мода изменился во время обновления".into());
                }
            } else {
                if !meta.file_type().is_file() || hash_file(&dest)? != hash.to_ascii_lowercase() {
                    return Err(format!("Файл {name} уже существует и отличается"));
                }
                continue;
            }
        }
        progress(&format!("Мод: {name}"));
        let temp = stage.path().join(format!("{}", staged.len()));
        let cached = cache.map(|c| c.join(hash.to_ascii_lowercase()));
        let from_cache = if let Some(path) = cached.as_ref() {
            if fs::symlink_metadata(path)
                .map(|m| m.file_type().is_file())
                .unwrap_or(false)
                && hash_file(path)
                    .map(|h| h == hash.to_ascii_lowercase())
                    .unwrap_or(false)
            {
                fs::copy(path, &temp).map_err(err)?;
                if hash_file(&temp)? != hash.to_ascii_lowercase() {
                    return Err("Повреждён файл кэша мода".into());
                }
                progress(&format!("Из кэша: {name}"));
                true
            } else {
                false
            }
        } else {
            false
        };
        if !from_cache {
            downloads.push(ModDownload {
                name: name.to_owned(),
                url: url.to_owned(),
                hash: hash.to_ascii_lowercase(),
                temp: temp.clone(),
            });
            if let Some(path) = cached {
                cache_writes.push((temp.clone(), path));
            }
        }
        staged.push((temp, dest));
    }
    download_mods(downloads)?;
    for (source, path) in cache_writes {
        if let Some(dir) = path.parent() {
            // Never link cached files to a profile: editing a mod in one instance must not alter another.
            let _ = (|| -> std::io::Result<()> {
                let temp = tempfile::NamedTempFile::new_in(dir)?;
                fs::copy(source, temp.path())?;
                temp.persist(&path).map_err(|e| e.error)?;
                Ok(())
            })();
        }
    }
    // Move the exact previously scanned file to an independent backup only after
    // every new file is downloaded and verified. Roll back on any promotion error.
    let backup = if let Some(old) = old {
        let old_path = dir.join(&old.name);
        if !fs::symlink_metadata(&old_path)
            .map_err(err)?
            .file_type()
            .is_file()
            || hash_file(&old_path)? != old.hash
        {
            return Err("Файл мода изменился во время обновления".into());
        }
        let backup_root = dir
            .parent()
            .ok_or("Некорректная папка модов")?
            .join(".horizon-mod-backups");
        fs::create_dir_all(&backup_root).map_err(err)?;
        if !fs::symlink_metadata(&backup_root)
            .map_err(err)?
            .file_type()
            .is_dir()
        {
            return Err("Папка резервных копий небезопасна".into());
        }
        let folder = backup_root.join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&folder).map_err(err)?;
        let path = folder.join(&old.name);
        fs::rename(&old_path, &path).map_err(err)?;
        Some((old_path, path))
    } else {
        None
    };
    let mut created: Vec<PathBuf> = Vec::new();
    let promote = (|| -> Result<()> {
        for (temp, dest) in staged {
            fs::hard_link(temp, &dest).map_err(err)?;
            created.push(dest);
        }
        crate::mod_state::save(dir, &records)?;
        Ok(())
    })();
    if let Err(e) = promote {
        for path in &created {
            let _ = fs::remove_file(path);
        }
        if let Some((original, backup)) = &backup {
            if let Err(restore) = fs::rename(backup, original) {
                return Err(format!(
                    "{e}; не удалось восстановить прежний файл: {restore}. Резервная копия: {}",
                    backup.display()
                ));
            }
        }
        return Err(e);
    }
    Ok(created.len())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compatibility_requires_both_loader_and_game() {
        let v = serde_json::json!({"game_versions":["1.20.1"],"loaders":["fabric"]});
        assert!(compatible(&v, "1.20.1", "fabric"));
        assert!(!compatible(&v, "1.20.2", "fabric"));
        assert!(!compatible(&v, "1.20.1", "forge"));
    }
    #[test]
    fn rejects_bad_file_metadata() {
        let v = serde_json::json!({"filename":"../evil.jar","url":"https://cdn.modrinth.com/x","hashes":{"sha512":"a".repeat(128)}});
        assert!(metadata(&v).is_err());
        assert!(!valid_id("../evil"));
    }
    #[test]
    fn detects_existing_versions_and_declared_conflicts() {
        let planned = serde_json::json!({"project_id":"root","id":"new", "dependencies":[{"dependency_type":"incompatible","project_id":"other"}]});
        let old = serde_json::json!({"project_id":"root","id":"old"});
        let other = serde_json::json!({"project_id":"other","id":"v"});
        assert!(check_existing(&[planned.clone()], &[old]).is_err());
        assert!(check_existing(&[planned.clone()], &[other]).is_err());
        assert!(check_existing(&[planned.clone()], &[planned]).is_ok());
    }
    #[test]
    fn stages_independent_mod_files_concurrently_and_verifies_hashes() {
        use std::{
            net::TcpListener,
            sync::{
                atomic::{AtomicUsize, Ordering},
                Arc,
            },
            time::Duration,
        };
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let a = active.clone();
        let p = peak.clone();
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let a = a.clone();
                let p = p.clone();
                workers.push(std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let count = a.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(count, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(200));
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
                    )
                    .unwrap();
                    a.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            for w in workers {
                w.join().unwrap();
            }
        });
        let hash = format!("{:x}", Sha512::digest(b"hello"));
        let files = (0..4)
            .map(|i| ModDownload {
                name: format!("{i}.jar"),
                url: format!("http://{addr}/{i}"),
                hash: hash.clone(),
                temp: dir.path().join(format!("{i}")),
            })
            .collect();
        download_mods(files).unwrap();
        server.join().unwrap();
        assert!(peak.load(Ordering::SeqCst) > 1);
        for i in 0..4 {
            assert_eq!(fs::read(dir.path().join(format!("{i}"))).unwrap(), b"hello");
        }
    }
    #[test]
    fn resolves_required_dependencies_and_cycles() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let root = serde_json::json!({"id":"vroot","project_id":"root","game_versions":["1.20.1"],"loaders":["fabric"],"dependencies":[{"dependency_type":"required","project_id":"dep","version_id":"vdep"}],"files":[{"filename":"root.jar"}]});
        let dep = serde_json::json!({"id":"vdep","project_id":"dep","game_versions":["1.20.1"],"loaders":["fabric"],"dependencies":[{"dependency_type":"required","project_id":"root","version_id":"vroot"}],"files":[{"filename":"dep.jar"}]});
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.starts_with("GET /project/root/version?") {
                    serde_json::json!([root.clone()]).to_string()
                } else if req.starts_with("GET /version/vdep ") {
                    dep.to_string()
                } else if req.starts_with("GET /version/vroot ") {
                    root.to_string()
                } else {
                    panic!("unexpected request {req}")
                };
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        let mut resolver = Resolver {
            client: Client::builder().no_proxy().build().unwrap(),
            api: format!("http://{addr}"),
            minecraft: "1.20.1",
            loader: "fabric",
            seen: HashMap::new(),
            files: Vec::new(),
            versions: Vec::new(),
        };
        resolver.resolve(Some("root"), None).unwrap();
        assert_eq!(resolver.seen.len(), 2);
        assert_eq!(resolver.files.len(), 2);
        assert_eq!(resolver.files[0]["filename"], "dep.jar");
        worker.join().unwrap();
    }
}
