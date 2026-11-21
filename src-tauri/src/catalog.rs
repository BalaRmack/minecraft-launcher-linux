//! Modrinth catalog and installed-file identification. All public operations run off the UI thread.
use crate::{client, err, modrinth, safe_mod_name, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Cache(Mutex<HashMap<String, (Instant, Value)>>);
impl Cache {
    fn get(&self, key: &str) -> Option<Value> {
        let mut entries = self.0.lock().ok()?;
        entries.retain(|_, (when, _)| when.elapsed() < Duration::from_secs(600));
        entries.get(key).map(|(_, value)| value.clone())
    }
    fn insert(&self, key: String, value: Value) {
        if let Ok(mut entries) = self.0.lock() {
            if entries.len() >= 128 {
                if let Some(oldest) = entries
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(k, _)| k.clone())
                {
                    entries.remove(&oldest);
                }
            }
            entries.insert(key, (Instant::now(), value));
        }
    }
}
#[derive(Deserialize, Serialize)]
pub struct Search {
    pub query: String,
    pub version: String,
    pub loader: String,
    pub category: String,
    pub sort: String,
    pub offset: u32,
}
fn valid_token(s: &str) -> bool {
    s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}
fn facets(p: &Search) -> Result<Vec<Vec<String>>> {
    if p.query.len() > 100
        || p.offset > 10000
        || !valid_token(&p.version)
        || !valid_token(&p.category)
        || !["", "fabric", "forge", "neoforge", "quilt"].contains(&p.loader.as_str())
        || !["relevance", "downloads", "follows", "updated", "newest"].contains(&p.sort.as_str())
    {
        return Err("Недопустимый фильтр каталога".into());
    }
    let mut f = vec![vec!["project_type:mod".into()]];
    if !p.version.is_empty() {
        f.push(vec![format!("versions:{}", p.version)]);
    }
    if !p.loader.is_empty() {
        f.push(vec![format!("categories:{}", p.loader)]);
    }
    if !p.category.is_empty() {
        f.push(vec![format!("categories:{}", p.category)]);
    }
    Ok(f)
}
fn cached(cache: &Cache, key: String, fetch: impl FnOnce() -> Result<Value>) -> Result<Value> {
    if let Some(value) = cache.get(&key) {
        return Ok(value);
    }
    let value = fetch()?;
    cache.insert(key, value.clone());
    Ok(value)
}
pub fn search(cache: &Cache, p: Search) -> Result<Value> {
    let facets = facets(&p)?;
    let key = format!("search:{}", serde_json::to_string(&p).map_err(err)?);
    cached(cache, key, || {
        client()?
            .get("https://api.modrinth.com/v2/search")
            .query(&[
                ("query", p.query),
                ("index", p.sort),
                ("offset", p.offset.to_string()),
                ("limit", "20".to_owned()),
                ("facets", serde_json::to_string(&facets).map_err(err)?),
            ])
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .json()
            .map_err(err)
    })
}
pub fn categories(cache: &Cache) -> Result<Value> {
    cached(cache, "categories".into(), || {
        let all: Value = client()?
            .get("https://api.modrinth.com/v2/tag/category")
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .json()
            .map_err(err)?;
        Ok(Value::Array(
            all.as_array()
                .ok_or("Некорректный список категорий")?
                .iter()
                .filter(|v| {
                    v["project_type"] == "mod" && v["name"].as_str().is_some_and(valid_token)
                })
                .cloned()
                .collect(),
        ))
    })
}
pub fn details(cache: &Cache, project: &str, version: &str, loader: &str) -> Result<Value> {
    if !valid_token(project)
        || project.is_empty()
        || !valid_token(version)
        || !["", "vanilla", "fabric", "forge", "neoforge", "quilt"].contains(&loader)
    {
        return Err("Некорректные параметры проекта".into());
    }
    let key = format!("details:{project}:{version}:{loader}");
    cached(cache, key, || {
        let c = client()?;
        let project_data: Value = c
            .get(format!("https://api.modrinth.com/v2/project/{project}"))
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .json()
            .map_err(err)?;
        let members: Value =
            if let Some(team) = project_data["team"].as_str().filter(|s| valid_token(s)) {
                c.get(format!("https://api.modrinth.com/v2/team/{team}/members"))
                    .send()
                    .ok()
                    .and_then(|r| r.error_for_status().ok())
                    .and_then(|r| r.json().ok())
                    .unwrap_or(json!([]))
            } else {
                json!([])
            };
        let available: Value = if !version.is_empty() && loader != "" && loader != "vanilla" {
            let versions: Value = c
                .get(format!(
                    "https://api.modrinth.com/v2/project/{project}/version"
                ))
                .query(&[
                    (
                        "game_versions",
                        serde_json::to_string(&[version]).map_err(err)?,
                    ),
                    ("loaders", serde_json::to_string(&[loader]).map_err(err)?),
                ])
                .send()
                .map_err(err)?
                .error_for_status()
                .map_err(err)?
                .json()
                .map_err(err)?;
            Value::Array(
                versions
                    .as_array()
                    .ok_or("Некорректный список версий")?
                    .iter()
                    .take(25)
                    .cloned()
                    .collect(),
            )
        } else {
            json!([])
        };
        let compatible = available
            .as_array()
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or(Value::Null);
        let available: Vec<Value> = available.as_array().unwrap().iter().map(|v| json!({"id":v["id"],"version_number":v["version_number"],"date_published":v["date_published"]})).collect();
        Ok(
            json!({"project":project_data,"members":members,"compatible_version":compatible,"available_versions":available}),
        )
    })
}
fn files_lookup(cache: &Cache, hashes: &[String]) -> Result<Value> {
    let key = format!("files:{}", hashes.join(","));
    cached(cache, key, || {
        client()?
            .post("https://api.modrinth.com/v2/version_files")
            .json(&json!({"hashes":hashes,"algorithm":"sha512"}))
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .json()
            .map_err(err)
    })
}
/// Recognizes only exact hashes from Modrinth; unknown local jars remain unknown.
pub fn inspect(cache: &Cache, dir: &Path, minecraft: &str, loader: &str) -> Result<Value> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(json!([]));
    }
    for entry in fs::read_dir(dir).map_err(err)? {
        let entry = entry.map_err(err)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type().map_err(err)?.is_file() && safe_mod_name(&name) {
            files.push((name, modrinth::hash_file(&entry.path())?));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut versions = serde_json::Map::new();
    for chunk in files.chunks(100) {
        let hashes: Vec<String> = chunk.iter().map(|(_, h)| h.clone()).collect();
        if let Some(map) = files_lookup(cache, &hashes)?.as_object() {
            versions.extend(map.clone());
        }
    }
    let project_ids: Vec<String> = versions
        .values()
        .filter_map(|v| v["project_id"].as_str())
        .filter(|s| valid_token(s))
        .map(str::to_owned)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut projects = HashMap::new();
    for chunk in project_ids.chunks(100) {
        let ids = serde_json::to_string(chunk).map_err(err)?;
        let result: Value = client()?
            .get("https://api.modrinth.com/v2/projects")
            .query(&[("ids", ids)])
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .json()
            .map_err(err)?;
        if let Some(arr) = result.as_array() {
            for p in arr {
                if let Some(id) = p["id"].as_str() {
                    projects.insert(id.to_owned(), p.clone());
                }
            }
        }
    }
    let enabled: Vec<&Value> = files
        .iter()
        .filter(|(n, _)| n.ends_with(".jar"))
        .filter_map(|(_, h)| versions.get(h))
        .collect();
    let mut rows = Vec::new();
    for (name, hash) in files {
        let v = versions.get(&hash);
        let project = v.and_then(|v| v["project_id"].as_str());
        let mut warnings = Vec::new();
        if let Some(v) = v {
            if !v["game_versions"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x == minecraft))
                || !v["loaders"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x == loader))
            {
                warnings.push("Версия или загрузчик не совпадают со сборкой".to_string());
            }
            if name.ends_with(".jar") {
                if let Some(deps) = v["dependencies"].as_array() {
                    for d in deps {
                        let p = d["project_id"].as_str();
                        let ver = d["version_id"].as_str();
                        let present = enabled.iter().any(|candidate| {
                            p.map_or(true, |x| candidate["project_id"] == x)
                                && ver.map_or(true, |x| candidate["id"] == x)
                                && (p.is_some() || ver.is_some())
                        });
                        if d["dependency_type"] == "required" && !present {
                            warnings.push(format!(
                                "Зависимость {} не найдена среди распознанных включённых модов",
                                p.or(ver).unwrap_or("без идентификатора")
                            ));
                        }
                        if d["dependency_type"] == "incompatible" && present {
                            warnings.push(format!(
                                "Конфликт с {}",
                                p.or(ver).unwrap_or("другим модом")
                            ));
                        }
                    }
                }
            }
        }
        rows.push(json!({"name":name,"enabled":name.ends_with(".jar"),"version":v,"project":project.and_then(|p|projects.get(p)),"warnings":warnings}));
    }
    Ok(Value::Array(rows))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_validate_and_construct_api_facets() {
        let s = Search {
            query: "map".into(),
            version: "1.20.1".into(),
            loader: "fabric".into(),
            category: "adventure".into(),
            sort: "downloads".into(),
            offset: 20,
        };
        let f = facets(&s).unwrap();
        assert_eq!(f[1][0], "versions:1.20.1");
        assert_eq!(f[2][0], "categories:fabric");
        let mut bad = s;
        bad.loader = "../../x".into();
        assert!(facets(&bad).is_err());
    }
}
