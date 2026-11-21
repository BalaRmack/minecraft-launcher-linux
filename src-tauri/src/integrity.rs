//! Offline validation of files described by the local version metadata.
use crate::{err, Result};
use mc_launcher_core::{
    install::{
        assets::{asset_index_path, plan_asset_object_downloads_from_index, AssetIndexJson},
        vanilla::plan_vanilla_downloads,
    },
    launcher::Launcher,
    net::download::{should_skip_existing, DownloadTask},
};
use std::{fs, path::Path};

fn check(task: &DownloadTask) -> Result<()> {
    let meta =
        fs::metadata(&task.destination).map_err(|_| format!("Отсутствует {}", task.label))?;
    if !meta.is_file() || meta.len() == 0 {
        return Err(format!("Файл {} пуст или не является файлом", task.label));
    }
    // Some loader libraries are published without a checksum; existence is the only available check.
    if task.checksum.is_some() && !should_skip_existing(task).map_err(err)? {
        return Err(format!("Повреждён {}", task.label));
    }
    Ok(())
}
pub fn verify(dir: &Path, version_id: &str) -> Result<()> {
    let version = Launcher::new(dir).load_version(version_id).map_err(err)?;
    if version.downloads.get("client").is_none() {
        return Err("В метаданных нет клиента Minecraft".into());
    }
    let plan = plan_vanilla_downloads(&version, dir).map_err(err)?;
    for task in &plan.tasks {
        check(task)?;
    }
    if let Some(index) = &version.asset_index {
        let bytes = fs::read(asset_index_path(dir, &index.id)).map_err(err)?;
        let index: AssetIndexJson = serde_json::from_slice(&bytes).map_err(err)?;
        for task in &plan_asset_object_downloads_from_index(&index, dir).tasks {
            check(task)?;
        }
    }
    let natives = mc_launcher_core::install::natives::natives_directory(dir, version_id);
    if !natives.is_dir() {
        return Err("Отсутствует каталог нативных библиотек".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_requires_file_and_checksum() {
        use mc_launcher_core::net::download::Checksum;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.jar");
        let task = DownloadTask {
            url: String::new(),
            destination: path.clone(),
            checksum: Some(Checksum::Sha1(
                "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".into(),
            )),
            label: "client".into(),
        };
        assert!(check(&task).is_err());
        fs::write(&path, b"hello").unwrap();
        assert!(check(&task).is_ok());
        fs::write(&path, b"wrong").unwrap();
        assert!(check(&task).is_err());
    }
}
