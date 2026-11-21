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
/// Downloads all required files before promoting any of them. Never overwrites existing mods.
pub fn install(
    dir: &Path,
    project: &str,
    minecraft: &str,
    loader: &str,
    progress: impl Fn(&str),
) -> Result<usize> {
    let mut resolver = Resolver {
        client: client()?,
        api: "https://api.modrinth.com/v2".into(),
        minecraft,
        loader,
        seen: HashMap::new(),
        files: Vec::new(),
    };
    resolver.resolve(Some(project), None)?;
    fs::create_dir_all(dir).map_err(err)?;
    let stage = tempfile::tempdir_in(dir).map_err(err)?;
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut names = HashMap::new();
    for file in &resolver.files {
        let (name, url, hash) = metadata(file)?;
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
            if !meta.file_type().is_file() || hash_file(&dest)? != hash.to_ascii_lowercase() {
                return Err(format!("Файл {name} уже существует и отличается"));
            }
            continue;
        }
        progress(&format!("Мод: {name}"));
        let mut response = resolver
            .client
            .get(url)
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?;
        if response.content_length().unwrap_or(0) > MAX_FILE {
            return Err("Файл мода слишком велик".into());
        }
        let temp = stage.path().join(format!("{}", staged.len()));
        let mut out = fs::File::create(&temp).map_err(err)?;
        let mut digest = Sha512::new();
        let mut size = 0u64;
        let mut buf = [0u8; 65536];
        loop {
            let n = response.read(&mut buf).map_err(err)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > MAX_FILE {
                return Err("Файл мода слишком велик".into());
            }
            digest.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(err)?;
        }
        if format!("{:x}", digest.finalize()) != hash.to_ascii_lowercase() {
            return Err(format!("Контрольная сумма {name} не совпадает"));
        }
        staged.push((temp, dest));
    }
    let mut created = Vec::new();
    for (temp, dest) in staged {
        if let Err(e) = fs::hard_link(temp, &dest) {
            for path in created {
                let _ = fs::remove_file(path);
            }
            return Err(err(e));
        }
        created.push(dest);
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
        };
        resolver.resolve(Some("root"), None).unwrap();
        assert_eq!(resolver.seen.len(), 2);
        assert_eq!(resolver.files.len(), 2);
        assert_eq!(resolver.files[0]["filename"], "dep.jar");
        worker.join().unwrap();
    }
}
