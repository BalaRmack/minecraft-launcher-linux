//! Loader versions from official metadata; the chosen version is revalidated before copying.
use crate::{client, err, Result};
use mc_launcher_core::loader::{forge, neoforge};
use serde_json::Value;

fn safe_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 100
        && !v.starts_with('.')
        && !v.contains("..")
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}
fn neo_prefix(game: &str) -> Option<String> {
    if let Some(legacy) = game.strip_prefix("1.") {
        let mut p = legacy.split('.');
        let major = p.next()?;
        let minor = p.next().unwrap_or("0");
        if p.next().is_some()
            || !major.bytes().all(|c| c.is_ascii_digit())
            || !minor.bytes().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        return Some(format!("{major}.{minor}."));
    }
    let mut p = game.split('.');
    let year = p.next()?;
    let release = p.next()?;
    let patch = p.next().unwrap_or("0");
    if p.next().is_some()
        || year.parse::<u32>().ok()? < 26
        || !release.bytes().all(|c| c.is_ascii_digit())
        || !patch.bytes().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(format!("{year}.{release}.{patch}."))
}
pub fn versions(game: &str, loader: &str) -> Result<Vec<String>> {
    if !safe_version(game) {
        return Err("Некорректная версия Minecraft".into());
    }
    let list = match loader {
        "vanilla" => vec![],
        "fabric" | "quilt" => {
            let base = if loader == "fabric" {
                "https://meta.fabricmc.net/v2"
            } else {
                "https://meta.quiltmc.org/v3"
            };
            let data: Value = client()?
                .get(format!("{base}/versions/loader/{game}"))
                .send()
                .map_err(err)?
                .error_for_status()
                .map_err(err)?
                .json()
                .map_err(err)?;
            data.as_array()
                .ok_or("Некорректный ответ загрузчика")?
                .iter()
                .filter_map(|v| v["loader"]["version"].as_str())
                .map(str::to_owned)
                .collect()
        }
        "forge" => {
            let prefix = format!("{game}-");
            forge::list_forge_versions()
                .map_err(err)?
                .into_iter()
                .rev()
                .filter(|x| x.starts_with(&prefix))
                .collect()
        }
        "neoforge" => {
            let prefix = neo_prefix(game).ok_or("NeoForge не поддерживает выбранную версию")?;
            neoforge::list_neoforge_versions()
                .map_err(err)?
                .into_iter()
                .rev()
                .filter(|x| x.starts_with(&prefix))
                .collect()
        }
        _ => return Err("Неизвестный загрузчик".into()),
    };
    let mut list: Vec<String> = list.into_iter().filter(|v| safe_version(v)).collect();
    list.dedup();
    Ok(list)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid_loader_versions_and_neo_mapping() {
        assert!(safe_version("1.20.1-47.4.0"));
        assert!(!safe_version("../../x"));
        assert_eq!(neo_prefix("1.21.1").as_deref(), Some("21.1."));
        assert_eq!(neo_prefix("1.21").as_deref(), Some("21.0."));
        assert_eq!(neo_prefix("24w01a"), None);
    }
}
