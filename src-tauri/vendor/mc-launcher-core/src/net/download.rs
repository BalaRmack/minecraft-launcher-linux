//! Download plans and execution.

use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
};

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

/// Executes a download plan in order.
///
/// Existing files with matching checksums are skipped. Each completed SHA-1
/// download is verified before the next task begins.
///
/// # Errors
///
/// Returns [`crate::LauncherError`] for network, filesystem, or checksum
/// failures.
pub fn execute_plan(plan: &DownloadPlan, reporter: &mut dyn ProgressReporter) -> Result<()> {
    let client = super::http::client()?;
    for task in &plan.tasks {
        if reporter.is_cancelled() {
            return Err(LauncherError::Other {
                message: "Установка отменена".into(),
            });
        }
        if should_skip_existing(task)? {
            reporter.report(ProgressEvent::TaskSkipped {
                label: task.label.clone(),
                reason: if task.checksum.is_some() {
                    SkipReason::ChecksumMatched
                } else {
                    SkipReason::FileExistsWithoutChecksum
                },
            });
            continue;
        }

        reporter.report(ProgressEvent::TaskStarted {
            label: task.label.clone(),
            path: task.destination.clone(),
        });

        if let Some(parent) = task.destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let parent = task
            .destination
            .parent()
            .ok_or_else(|| LauncherError::Other {
                message: "download destination has no parent".into(),
            })?;
        let mut last_error = None;
        for attempt in 0..3 {
            let result = (|| -> Result<()> {
                let mut response = client.get(&task.url).send()?.error_for_status()?;
                // Write beside the destination; only promote a fully received and verified file.
                let mut file = tempfile::NamedTempFile::new_in(parent)?;
                let total = response.content_length();
                let mut received = 0u64;
                let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
                let mut buf = [0u8; 65536];
                loop {
                    if reporter.is_cancelled() {
                        return Err(LauncherError::Other {
                            message: "Установка отменена".into(),
                        });
                    }
                    let count = response.read(&mut buf)?;
                    if count == 0 {
                        break;
                    }
                    file.write_all(&buf[..count])?;
                    received += count as u64;
                    if last.elapsed() >= std::time::Duration::from_millis(250) {
                        reporter.report(ProgressEvent::BytesReceived {
                            label: task.label.clone(),
                            received,
                            total,
                        });
                        last = std::time::Instant::now();
                    }
                }
                if reporter.is_cancelled() {
                    return Err(LauncherError::Other {
                        message: "Установка отменена".into(),
                    });
                }
                let actual = match &task.checksum {
                    Some(Checksum::Sha1(_)) => Some(sha1_file(file.path())?),
                    Some(Checksum::Sha256(_)) => Some(sha256_file(file.path())?),
                    None => None,
                };
                if let (Some(expected), Some(actual)) = (
                    task.checksum.as_ref().map(|c| match c {
                        Checksum::Sha1(s) | Checksum::Sha256(s) => s,
                    }),
                    actual,
                ) {
                    if *expected != actual {
                        return Err(LauncherError::ChecksumMismatch {
                            path: task.destination.clone(),
                            expected: expected.clone(),
                            actual,
                        });
                    }
                }
                file.persist(&task.destination).map_err(|e| e.error)?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(e) => {
                    last_error = Some(e);
                    if reporter.is_cancelled() {
                        break;
                    }
                    if attempt < 2 {
                        std::thread::sleep(std::time::Duration::from_millis(400 * (attempt + 1)));
                    }
                }
            }
        }
        if reporter.is_cancelled() {
            return Err(LauncherError::Other {
                message: "Установка отменена".into(),
            });
        }
        if let Some(error) = last_error {
            return Err(error);
        }

        reporter.report(ProgressEvent::TaskFinished {
            label: task.label.clone(),
        });
    }
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
