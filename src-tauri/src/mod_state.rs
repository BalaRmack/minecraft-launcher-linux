//! Profile-local Modrinth identities, always joined with current files by SHA-512.
use crate::{client, err, modrinth, safe_mod_name, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
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
// Only an explicit Modrinth project link in the mod's own metadata is used;
// a loader mod-id or filename alone is not a Modrinth project identifier.
fn explicit_project_link(path: &Path) -> Option<(String, Option<String>)> {
    let file = fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    for name in ["fabric.mod.json", "quilt.mod.json"] {
        let mut entry = match zip.by_name(name) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if entry.size() > 65536 {
            continue;
        }
        let mut text = String::new();
        entry.read_to_string(&mut text).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        let (url, version) = if name == "fabric.mod.json" {
            (
                value["contact"]["homepage"].as_str(),
                value["version"].as_str(),
            )
        } else {
            (
                value["quilt_loader"]["metadata"]["contact"]["homepage"].as_str(),
                value["quilt_loader"]["version"].as_str(),
            )
        };
        if let Some(url) = url {
            let slug = url
                .strip_prefix("https://modrinth.com/mod/")?
                .trim_end_matches('/');
            if !slug.is_empty()
                && slug.len() <= 64
                && slug
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Some((slug.to_owned(), version.map(str::to_owned)));
            }
        }
    }
    None
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
    // Unknown manually added Fabric/Quilt jars may identify their project with
    // an explicit Modrinth homepage. Resolve the slug to a canonical project ID.
    for file in files.iter_mut().filter(|f| f.version.is_none()) {
        let Some((slug, version)) = explicit_project_link(&dir.join(&file.name)) else {
            continue;
        };
        let found = (|| -> Result<Value> {
            client()?
                .get(format!("https://api.modrinth.com/v2/project/{slug}"))
                .send()
                .map_err(err)?
                .error_for_status()
                .map_err(err)?
                .json()
                .map_err(err)
        })();
        match found {
            Ok(project) if project["project_type"] == "mod" && project["id"].is_string() => {
                file.version = Some(
                    json!({"project_id":project["id"],"version_number":version,"identity_source":"mod_metadata"}),
                )
            }
            Err(_) => complete = false,
            _ => (),
        }
    }
    Ok((files, complete))
}
pub fn status(dir: &Path, minecraft: &str, loader: &str) -> Result<Value> {
    let (files, complete) = scan(dir)?;
    let known: Vec<String> = files
        .iter()
        .filter(|f| f.version.as_ref().is_some_and(|v| v["id"].is_string()))
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_requires_current_file_hash_and_is_profile_scoped() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a/game/mods");
        let b = root.path().join("b/game/mods");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("renamed.jar"), b"verified").unwrap();
        let hash = modrinth::hash_file(&a.join("renamed.jar")).unwrap();
        let mut records = HashMap::new();
        records.insert(hash, json!({"project_id":"project","id":"old"}));
        save(&a, &records).unwrap();
        assert_eq!(
            scan(&a).unwrap().0[0].version.as_ref().unwrap()["project_id"],
            "project"
        );
        assert!(scan(&b).unwrap().0.is_empty());
        fs::rename(a.join("renamed.jar"), a.join("renamed.jar.disabled")).unwrap();
        assert_eq!(scan(&a).unwrap().0[0].name, "renamed.jar.disabled");
        fs::remove_file(a.join("renamed.jar.disabled")).unwrap();
        assert!(scan(&a).unwrap().0.is_empty());
    }
    #[test]
    fn only_explicit_modrinth_link_from_mod_metadata_is_used() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mod.jar");
        let f = fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(f);
        zip.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(br#"{"id":"not-a-project","version":"1.0","contact":{"homepage":"https://modrinth.com/mod/xaeros-minimap"}}"#).unwrap();
        zip.finish().unwrap();
        assert_eq!(
            explicit_project_link(&path),
            Some(("xaeros-minimap".into(), Some("1.0".into())))
        );
    }
}
