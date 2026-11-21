//! Managed Java runtimes. All network and process work is called on a blocking worker.
use crate::{err, managed_child, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;

fn parse_java_version(output: &str) -> Option<u32> {
    let version = output
        .lines()
        .find(|line| line.contains("version"))?
        .split('"')
        .nth(1)?;
    let mut parts = version.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}
fn java_major(executable: &Path) -> Option<u32> {
    let output = Command::new(executable).arg("-version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_java_version(&format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    ))
}
fn safe_path(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}
fn safe_link(path: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth = path
        .parent()
        .map(|p| {
            p.components()
                .filter(|c| matches!(c, Component::Normal(_)))
                .count()
        })
        .unwrap_or(0);
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => (),
            Component::ParentDir if depth > 1 => depth -= 1,
            _ => return false,
        }
    }
    // A link must stay within the top-level JRE directory.
    depth > 0
}

fn extract(archive: &Path, target: &Path) -> Result<PathBuf> {
    let file = fs::File::open(archive).map_err(err)?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut total = 0u64;
    let mut roots = std::collections::HashSet::new();
    for item in archive.entries().map_err(err)? {
        let mut entry = item.map_err(err)?;
        let path = entry.path().map_err(err)?.into_owned();
        if !safe_path(&path) {
            return Err("Небезопасный путь в архиве Java".into());
        }
        let root = path
            .components()
            .find_map(|c| {
                if let Component::Normal(c) = c {
                    Some(c.to_os_string())
                } else {
                    None
                }
            })
            .ok_or("Пустой путь в архиве Java")?;
        roots.insert(root);
        let kind = entry.header().entry_type();
        if kind.is_symlink() {
            let link = entry
                .link_name()
                .map_err(err)?
                .ok_or("Пустая ссылка в архиве Java")?;
            if !safe_link(&path, &link) {
                return Err("Небезопасная ссылка в архиве Java".into());
            }
        } else if !kind.is_file() && !kind.is_dir() {
            return Err("Неизвестный тип файла в архиве Java".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Архив Java слишком велик")?;
        if total > MAX_UNPACKED {
            return Err("Архив Java слишком велик".into());
        }
        if !entry.unpack_in(target).map_err(err)? {
            return Err("Небезопасный путь в архиве Java".into());
        }
    }
    if roots.len() != 1 {
        return Err("Некорректная структура архива Java".into());
    }
    let root = target.join(roots.into_iter().next().ok_or("Архив Java пуст")?);
    if !root.join("bin/java").is_file() {
        return Err("В архиве отсутствует Java".into());
    }
    Ok(root)
}

/// Resolve either an explicit user executable or a compatible managed Temurin JRE.
pub fn resolve(
    root: &Path,
    configured: &str,
    major: u32,
    progress: impl Fn(&str),
) -> Result<PathBuf> {
    if !(8..=30).contains(&major) {
        return Err(format!("Неподдерживаемая версия Java: {major}"));
    }
    if configured != "auto" {
        let path = PathBuf::from(configured);
        return match java_major(&path) {
            Some(found) if found == major => Ok(path),
            Some(found) => Err(format!(
                "Нужна Java {major}, но выбрана Java {found}. Выберите auto или подходящую версию."
            )),
            None => Err(format!("Не удалось запустить Java: {configured}")),
        };
    }
    if java_major(Path::new("java")) == Some(major) {
        return Ok(PathBuf::from("java"));
    }
    let runtimes = managed_child(root, "runtime", true)?;
    let dest = managed_child(&runtimes, &format!("temurin-{major}"), false)?;
    let executable = dest.join("bin/java");
    if java_major(&executable) == Some(major) {
        return Ok(executable);
    }
    progress(&format!("Загрузка Java {major}"));
    let client = reqwest::blocking::Client::builder()
        .user_agent(
            "BalaRmack/Horizon/0.1.0 (https://github.com/BalaRmack/minecraft-launcher-linux)",
        )
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(err)?;
    let assets: serde_json::Value = client
        .get(format!(
            "https://api.adoptium.net/v3/assets/latest/{major}/hotspot"
        ))
        .query(&[
            ("architecture", "x64"),
            ("image_type", "jre"),
            ("os", "linux"),
            ("vendor", "eclipse"),
        ])
        .send()
        .map_err(err)?
        .error_for_status()
        .map_err(err)?
        .json()
        .map_err(err)?;
    let pkg = assets
        .as_array()
        .and_then(|a| a.first())
        .map(|a| &a["binary"]["package"])
        .ok_or_else(|| format!("Для Java {major} не найден Linux x86_64 runtime"))?;
    let url = pkg["link"].as_str().ok_or("Отсутствует ссылка на Java")?;
    let expected = pkg["checksum"]
        .as_str()
        .ok_or("Отсутствует контрольная сумма Java")?;
    if !url.starts_with("https://")
        || expected.len() != 64
        || !expected.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Некорректные метаданные Java".into());
    }
    let mut response = client
        .get(url)
        .send()
        .map_err(err)?
        .error_for_status()
        .map_err(err)?;
    if response.content_length().unwrap_or(0) > MAX_ARCHIVE {
        return Err("Архив Java слишком велик".into());
    }
    let mut file = tempfile::NamedTempFile::new_in(&runtimes).map_err(err)?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = response.read(&mut buffer).map_err(err)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_ARCHIVE {
            return Err("Архив Java слишком велик".into());
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n]).map_err(err)?;
    }
    if format!("{:x}", hasher.finalize()) != expected.to_ascii_lowercase() {
        return Err("Контрольная сумма Java не совпадает".into());
    }
    progress(&format!("Установка Java {major}"));
    let stage = tempfile::tempdir_in(&runtimes).map_err(err)?;
    let extracted = extract(file.path(), stage.path())?;
    if java_major(&extracted.join("bin/java")) != Some(major) {
        return Err("Загруженная Java не запустилась или имеет неверную версию".into());
    }
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(err)?;
    }
    fs::rename(extracted, &dest).map_err(err)?;
    Ok(executable)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_java_versions() {
        assert_eq!(
            parse_java_version("openjdk version \"21.0.4\" 2024"),
            Some(21)
        );
        assert_eq!(parse_java_version("java version \"1.8.0_402\""), Some(8));
        assert_eq!(parse_java_version("bad"), None);
    }
    #[test]
    fn rejects_archive_escape() {
        assert!(!safe_path(Path::new("../../x")));
        assert!(!safe_path(Path::new("/etc/passwd")));
        assert!(!safe_link(
            Path::new("jre/bin/java"),
            Path::new("../../../etc/passwd")
        ));
        assert!(!safe_link(
            Path::new("jre/bin/java"),
            Path::new("../../other/file")
        ));
        assert!(safe_link(
            Path::new("jre/bin/java"),
            Path::new("../lib/java")
        ));
    }
}
