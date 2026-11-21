//! Download plans and execution.

use std::{fs, io::Write, path::PathBuf};

use crate::{
    io::hash::sha1_file,
    progress::{ProgressEvent, ProgressReporter, SkipReason},
    LauncherError, Result,
};

/// Supported checksum validation methods for downloaded files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum {
    /// SHA-1 checksum.
    Sha1(String),
    /// SHA-256 checksum.
    Sha256(String),
}

/// One file download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadTask {
    /// Source URL.
    pub url: String,
    /// Destination path.
    pub destination: PathBuf,
    /// Optional checksum used for skip and validation decisions.
    pub checksum: Option<Checksum>,
    /// Human-readable task label reported in progress events.
    pub label: String,
}

/// A batch of download tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadPlan {
    /// Tasks to execute in order.
    pub tasks: Vec<DownloadTask>,
}

/// Returns whether an existing destination file can be reused.
///
/// # Errors
///
/// Returns [`crate::LauncherError`] if checksum calculation fails.
pub fn should_skip_existing(task: &DownloadTask) -> Result<bool> {
    if !task.destination.is_file() {
        return Ok(false);
    }

    match &task.checksum {
        Some(Checksum::Sha1(expected)) => Ok(sha1_file(&task.destination)? == *expected),
        Some(Checksum::Sha256(expected)) => Ok(sha256_file(&task.destination)? == *expected),
        // An unverified file could have been left by an older interrupted installation.
        None => Ok(false),
    }
}

/// Execute independent files concurrently using bounded asynchronous HTTP requests.
/// A plan is still a dependency boundary: indexes are completed before assets are planned.
pub fn execute_plan(plan: &DownloadPlan, reporter: &mut dyn ProgressReporter) -> Result<()> {
    use std::collections::HashMap;
    let mut unique = HashMap::<PathBuf, DownloadTask>::new();
    let mut tasks = Vec::new();
    for task in &plan.tasks {
        if let Some(previous) = unique.get(&task.destination) {
            if previous.checksum != task.checksum
                || (task.checksum.is_none() && previous.url != task.url)
            {
                return Err(LauncherError::Other {
                    message: "Conflicting downloads target the same path".into(),
                });
            }
        } else {
            unique.insert(task.destination.clone(), task.clone());
            tasks.push(task.clone());
        }
    }
    reporter.report(ProgressEvent::BatchStarted { total: tasks.len() });
    if reporter.is_cancelled() {
        return Err(cancelled());
    }
    let cache = reporter.cache_dir();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let client = reqwest::Client::builder()
            .user_agent(super::http::user_agent())
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(300))
            .build()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut jobs = tokio::task::JoinSet::new();
        let mut remaining = tasks.into_iter();
        let mut error = None;
        loop {
            while jobs.len() < 8 && error.is_none() && !reporter.is_cancelled() {
                let Some(task) = remaining.next() else { break };
                jobs.spawn(download_one(
                    client.clone(),
                    task,
                    cache.clone(),
                    tx.clone(),
                ));
            }
            while let Ok(event) = rx.try_recv() {
                reporter.report(event);
            }
            if reporter.is_cancelled() {
                jobs.abort_all();
                while jobs.join_next().await.is_some() {}
                return Err(cancelled());
            }
            if jobs.is_empty() {
                break;
            }
            match tokio::time::timeout(std::time::Duration::from_millis(100), jobs.join_next())
                .await
            {
                Ok(Some(Ok(Err(e)))) => {
                    error = Some(e);
                    jobs.abort_all();
                }
                Ok(Some(Err(e))) if !e.is_cancelled() => {
                    error = Some(LauncherError::Other {
                        message: format!("Download task failed: {e}"),
                    });
                    jobs.abort_all();
                }
                _ => (),
            }
            if error.is_some() {
                while jobs.join_next().await.is_some() {}
                break;
            }
        }
        while let Ok(event) = rx.try_recv() {
            reporter.report(event);
        }
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    })
}
fn cancelled() -> LauncherError {
    LauncherError::Other {
        message: "Установка отменена".into(),
    }
}
fn cache_key(cache: Option<&std::path::Path>, checksum: &Option<Checksum>) -> Option<PathBuf> {
    let (kind, hash) = match checksum.as_ref()? {
        Checksum::Sha1(h) => ("sha1", h),
        Checksum::Sha256(h) => ("sha256", h),
    };
    let expected = if kind == "sha1" { 40 } else { 64 };
    if hash.len() != expected || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let root = cache?;
    // Reject pre-existing symlinked cache directories, rather than writing outside the app tree.
    match fs::create_dir(root) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return None,
    }
    if !fs::symlink_metadata(root).ok()?.file_type().is_dir() {
        return None;
    }
    let dir = root.join(kind);
    match fs::create_dir(&dir) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return None,
    }
    if !fs::symlink_metadata(&dir).ok()?.file_type().is_dir() {
        return None;
    }
    Some(dir.join(hash.to_ascii_lowercase()))
}
async fn download_one(
    client: reqwest::Client,
    task: DownloadTask,
    cache: Option<PathBuf>,
    tx: std::sync::mpsc::Sender<ProgressEvent>,
) -> Result<()> {
    if should_skip_existing(&task)? {
        let _ = tx.send(ProgressEvent::TaskSkipped {
            label: task.label.clone(),
            reason: SkipReason::ChecksumMatched,
        });
        return Ok(());
    }
    let parent = task
        .destination
        .parent()
        .ok_or_else(|| LauncherError::Other {
            message: "download destination has no parent".into(),
        })?;
    fs::create_dir_all(parent)?;
    let cache_path = cache_key(cache.as_deref(), &task.checksum);
    if let Some(path) = &cache_path {
        let mut cached = task.clone();
        cached.destination = path.clone();
        if fs::symlink_metadata(path)
            .map(|m| m.file_type().is_file())
            .unwrap_or(false)
            && should_skip_existing(&cached).unwrap_or(false)
        {
            let temp = tempfile::NamedTempFile::new_in(parent)?;
            fs::copy(path, temp.path())?;
            if !should_skip_existing(&DownloadTask {
                destination: temp.path().to_path_buf(),
                ..task.clone()
            })? {
                return Err(LauncherError::Other {
                    message: "Повреждён файл кэша".into(),
                });
            }
            temp.persist(&task.destination).map_err(|e| e.error)?;
            let _ = tx.send(ProgressEvent::TaskSkipped {
                label: task.label.clone(),
                reason: SkipReason::ChecksumMatched,
            });
            return Ok(());
        }
    }
    let _ = tx.send(ProgressEvent::TaskStarted {
        label: task.label.clone(),
        path: task.destination.clone(),
    });
    let mut last_error = None;
    for attempt in 0..3 {
        let result: Result<()> = async {
            let mut response = client.get(&task.url).send().await?.error_for_status()?;
            let total = response.content_length();
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            let mut received = 0u64;
            let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
            while let Some(chunk) = response.chunk().await? {
                temp.write_all(&chunk)?;
                received += chunk.len() as u64;
                if last.elapsed() >= std::time::Duration::from_millis(250) {
                    let _ = tx.send(ProgressEvent::BytesReceived {
                        label: task.label.clone(),
                        received,
                        total,
                    });
                    last = std::time::Instant::now();
                }
            }
            let _ = tx.send(ProgressEvent::BytesReceived {
                label: task.label.clone(),
                received,
                total,
            });
            let actual = match &task.checksum {
                Some(Checksum::Sha1(_)) => Some(sha1_file(temp.path())?),
                Some(Checksum::Sha256(_)) => Some(sha256_file(temp.path())?),
                None => None,
            };
            if let (Some(expected), Some(actual)) = (
                task.checksum.as_ref().map(|c| match c {
                    Checksum::Sha1(s) | Checksum::Sha256(s) => s,
                }),
                actual,
            ) {
                if !expected.eq_ignore_ascii_case(&actual) {
                    return Err(LauncherError::ChecksumMismatch {
                        path: task.destination.clone(),
                        expected: expected.clone(),
                        actual,
                    });
                }
            }
            temp.persist(&task.destination).map_err(|e| e.error)?;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                last_error = None;
                break;
            }
            Err(e) => {
                last_error = Some(e);
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(400 * (attempt + 1))).await;
                }
            }
        }
    }
    if let Some(e) = last_error {
        return Err(e);
    }
    if let Some(path) = cache_path {
        // Cache writes are best-effort; profile copies never share mutable inodes.
        if let Some(parent) = path.parent() {
            let _ = (|| -> std::io::Result<()> {
                fs::create_dir_all(parent)?;
                let temp = tempfile::NamedTempFile::new_in(parent)?;
                fs::copy(&task.destination, temp.path())?;
                temp.persist(&path).map_err(|e| e.error)?;
                Ok(())
            })();
        }
    }
    let _ = tx.send(ProgressEvent::TaskFinished { label: task.label });
    Ok(())
}

fn sha256_file(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
