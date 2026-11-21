//! Profile-local Modrinth identities, always joined with current files by SHA-512.
use crate::{client, err, modrinth, safe_mod_name, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn ledger_path(dir: &Path) -> Result<PathBuf> {
    Ok(dir
        .parent()
        .ok_or("Некорректная папка модов")?
        .join(".horizon-modrinth.json"))
}
pub fn ledger(dir: &Path) -> Result<HashMap<String, Value>> {
    let path = ledger_path(dir)?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(e) => Err(err(e)),
        Ok(m) if m.file_type().is_file() => {
            serde_json::from_slice(&fs::read(path).map_err(err)?).map_err(err)
        }
        Ok(_) => Err("Метаданные модов не являются обычным файлом".into()),
    }
}
pub fn save(dir: &Path, records: &HashMap<String, Value>) -> Result<()> {
    let path = ledger_path(dir)?;
    let parent = path.parent().ok_or("Некорректная папка модов")?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(err)?;
    serde_json::to_writer(&mut tmp, records).map_err(err)?;
    tmp.flush().map_err(err)?;
    tmp.persist(path).map_err(err)?;
    Ok(())
}
#[derive(Clone)]
pub struct LocalMod {
    pub name: String,
    pub hash: String,
    pub version: Option<Value>,
}
/// Scan every time: no path, mtime, or remembered install flag is accepted as evidence.
pub fn scan(dir: &Path) -> Result<(Vec<LocalMod>, bool)> {
    let records = ledger(dir)?;
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok((files, true));
    }
    for entry in fs::read_dir(dir).map_err(err)? {
        let entry = entry.map_err(err)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !entry.file_type().map_err(err)?.is_file() || !safe_mod_name(&name) {
            continue;
        }
        let hash = modrinth::hash_file(&entry.path())?;
        files.push(LocalMod {
            name,
            version: records.get(&hash).cloned(),
            hash,
        });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let missing: Vec<String> = files
        .iter()
        .filter(|f| f.version.is_none())
        .map(|f| f.hash.clone())
        .collect();
    let mut complete = true;
    for chunk in missing.chunks(100) {
        let result = (|| -> Result<Value> {
            client()?
                .post("https://api.modrinth.com/v2/version_files")
                .json(&json!({"hashes":chunk,"algorithm":"sha512"}))
                .send()
                .map_err(err)?
                .error_for_status()
                .map_err(err)?
                .json()
                .map_err(err)
        })();
        match result {
            Ok(value) if value.is_object() => {
                for file in &mut files {
                    if file.version.is_none() {
                        file.version = value
                            .get(&file.hash)
                            .filter(|v| v["project_id"].is_string() && v["id"].is_string())
                            .cloned();
                    }
                }
            }
            _ => complete = false,
        }
    }
    Ok((files, complete))
}
pub fn status(dir: &Path, minecraft: &str, loader: &str) -> Result<Value> {
    let (files, complete) = scan(dir)?;
    let known: Vec<String> = files
        .iter()
        .filter(|f| f.version.is_some())
        .map(|f| f.hash.clone())
        .collect();
    let mut updates = serde_json::Map::new();
    let mut updates_checked = true;
    if loader != "vanilla" {
        for chunk in known.chunks(100) {
            let result = (|| -> Result<Value> {
                client()?.post("https://api.modrinth.com/v2/version_files/update").json(&json!({"hashes":chunk,"algorithm":"sha512","loaders":[loader],"game_versions":[minecraft]})).send().map_err(err)?.error_for_status().map_err(err)?.json().map_err(err)
            })();
            match result {
                Ok(v) if v.is_object() => updates.extend(v.as_object().unwrap().clone()),
                _ => updates_checked = false,
            }
        }
    }
    let rows:Vec<Value>=files.iter().map(|file|{
        let current=file.version.as_ref(); let update=updates.get(&file.hash).filter(|next| {
            let Some(old)=current else{return false};
            next["project_id"]==old["project_id"] && next["id"]!=old["id"] &&
            next["game_versions"].as_array().is_some_and(|a|a.iter().any(|v|v==minecraft)) &&
            next["loaders"].as_array().is_some_and(|a|a.iter().any(|v|v==loader)) &&
            next["date_published"].as_str().zip(old["date_published"].as_str()).is_some_and(|(a,b)|a>b)
        });
        json!({"name":file.name,"hash":file.hash,"enabled":file.name.ends_with(".jar"),"version":current,"update":update})
    }).collect();
    Ok(
        json!({"files":rows,"lookup_complete":complete,"unknown":files.iter().filter(|f|f.version.is_none()).count(),"updates_checked":updates_checked}),
    )
}
