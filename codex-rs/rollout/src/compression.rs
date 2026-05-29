use std::ffi::OsStr;
use std::fs::File;
use std::fs::Permissions;
use std::io;
use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tokio::io::AsyncBufReadExt;

const COMPRESSED_SUFFIX: &str = ".zst";
const TEMP_SUFFIX: &str = ".tmp";
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) async fn file_modified_time(path: &Path) -> io::Result<Option<time::OffsetDateTime>> {
    let Some(path) = existing_rollout_path(path).await else {
        return Ok(None);
    };
    let meta = tokio::fs::metadata(path).await?;
    let modified = meta.modified().ok();
    Ok(modified.map(time::OffsetDateTime::from))
}

/// Opens a rollout line reader that transparently handles plain `.jsonl` and `.jsonl.zst` files.
///
/// If the requested path disappears during a representation transition, this retries the matching
/// plain/compressed sibling once so callers do not need to know which representation is on disk.
pub async fn open_rollout_line_reader(path: &Path) -> io::Result<RolloutLineReader> {
    match open_rollout_line_reader_once(path).await {
        Ok(reader) => Ok(reader),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            match open_rollout_line_reader_once(path).await {
                Ok(reader) => Ok(reader),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    open_rollout_line_reader_alternate(path).await
                }
                Err(err) => Err(err),
            }
        }
        Err(err) => Err(err),
    }
}

pub(crate) async fn materialize_rollout_for_append(path: &Path) -> io::Result<PathBuf> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || materialize_rollout_for_append_blocking(path.as_path()))
        .await
        .map_err(io::Error::other)?
}

pub(crate) fn materialize_rollout_for_append_blocking(path: &Path) -> io::Result<PathBuf> {
    let plain_path = plain_rollout_path(path);
    if plain_path.exists() {
        return Ok(plain_path);
    }
    let compressed_path = compressed_rollout_path(plain_path.as_path());
    if !compressed_path.exists() {
        return Ok(plain_path);
    }

    let temp_path = temp_path_for(plain_path.as_path(), "decompress");
    if let Some(parent) = plain_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result: io::Result<()> = (|| {
        let permissions = std::fs::metadata(compressed_path.as_path())?.permissions();
        {
            let input = File::open(compressed_path.as_path())?;
            let mut decoder = zstd::stream::read::Decoder::new(input)?;
            let mut output = create_file_with_permissions(temp_path.as_path(), &permissions)?;
            io::copy(&mut decoder, &mut output)?;
            output.flush()?;
            output.sync_all()?;
        }
        match std::fs::hard_link(temp_path.as_path(), plain_path.as_path()) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => persist_temp_file_noclobber(temp_path.as_path(), plain_path.as_path())?,
        }
        let _ = std::fs::remove_file(temp_path.as_path());
        match std::fs::remove_file(compressed_path.as_path()) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp_path.as_path());
    }
    result?;
    Ok(plain_path)
}

fn persist_temp_file_noclobber(temp_path: &Path, destination: &Path) -> io::Result<()> {
    let temp_path = tempfile::TempPath::try_from_path(temp_path)?;
    match temp_path.persist_noclobber(destination) {
        Ok(()) => Ok(()),
        Err(err) if err.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err.error),
    }
}

pub(crate) fn compressed_rollout_path(path: &Path) -> PathBuf {
    if is_compressed_rollout_path(path) {
        return path.to_path_buf();
    }
    let mut file_name = path
        .file_name()
        .map(OsStr::to_os_string)
        .unwrap_or_else(|| OsStr::new("rollout.jsonl").to_os_string());
    file_name.push(COMPRESSED_SUFFIX);
    path.with_file_name(file_name)
}

/// Returns the plain `.jsonl` path for a plain or compressed rollout path.
pub fn plain_rollout_path(path: &Path) -> PathBuf {
    let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
        return path.to_path_buf();
    };
    let Some(plain_file_name) = file_name.strip_suffix(COMPRESSED_SUFFIX) else {
        return path.to_path_buf();
    };
    path.with_file_name(plain_file_name)
}

pub(crate) fn is_compressed_rollout_path(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.ends_with(".jsonl.zst"))
}

pub(crate) fn is_rollout_file_name(name: &str) -> bool {
    parse_rollout_file_name(name).is_some()
}

pub(crate) fn parse_rollout_file_name(name: &str) -> Option<&str> {
    let name = name.strip_suffix(COMPRESSED_SUFFIX).unwrap_or(name);
    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
        Some(name)
    } else {
        None
    }
}

pub(crate) fn should_skip_compressed_sibling(path: &Path) -> bool {
    is_compressed_rollout_path(path) && plain_rollout_path(path).exists()
}

/// Line-oriented rollout reader returned by [`open_rollout_line_reader`].
pub struct RolloutLineReader {
    inner: RolloutLineReaderInner,
}

enum RolloutLineReaderInner {
    Plain(tokio::io::Lines<tokio::io::BufReader<tokio::fs::File>>),
    Blocking(Option<BlockingLineReader>),
}

impl RolloutLineReader {
    /// Reads the next JSONL record from the rollout.
    pub async fn next_line(&mut self) -> io::Result<Option<String>> {
        match &mut self.inner {
            RolloutLineReaderInner::Plain(lines) => lines.next_line().await,
            RolloutLineReaderInner::Blocking(slot) => {
                let Some(mut reader) = slot.take() else {
                    return Err(io::Error::other("compressed rollout reader is busy"));
                };
                let (line, reader) =
                    tokio::task::spawn_blocking(move || (reader.next().transpose(), reader))
                        .await
                        .map_err(io::Error::other)?;
                *slot = Some(reader);
                line
            }
        }
    }
}

type BlockingLineReader = std::io::Lines<std::io::BufReader<Box<dyn Read + Send>>>;

/// Returns the existing rollout path, preferring the plain `.jsonl` file over
/// its `.jsonl.zst` compressed sibling.
pub async fn existing_rollout_path(path: &Path) -> Option<PathBuf> {
    let plain_path = plain_rollout_path(path);
    if tokio::fs::try_exists(plain_path.as_path())
        .await
        .unwrap_or(false)
    {
        return Some(plain_path);
    }
    let compressed_path = compressed_rollout_path(plain_path.as_path());
    if tokio::fs::try_exists(compressed_path.as_path())
        .await
        .unwrap_or(false)
    {
        return Some(compressed_path);
    }
    None
}

async fn open_rollout_line_reader_once(path: &Path) -> io::Result<RolloutLineReader> {
    let path = existing_rollout_path(path)
        .await
        .unwrap_or_else(|| path.to_path_buf());
    if is_compressed_rollout_path(path.as_path()) {
        return open_compressed_reader(path).await;
    }
    let file = tokio::fs::File::open(path).await?;
    Ok(RolloutLineReader {
        inner: RolloutLineReaderInner::Plain(tokio::io::BufReader::new(file).lines()),
    })
}

async fn open_rollout_line_reader_alternate(path: &Path) -> io::Result<RolloutLineReader> {
    let plain_path = plain_rollout_path(path);
    let compressed_path = compressed_rollout_path(plain_path.as_path());
    if is_compressed_rollout_path(path) {
        let file = tokio::fs::File::open(plain_path).await?;
        return Ok(RolloutLineReader {
            inner: RolloutLineReaderInner::Plain(tokio::io::BufReader::new(file).lines()),
        });
    }
    open_compressed_reader(compressed_path).await
}

async fn open_compressed_reader(path: PathBuf) -> io::Result<RolloutLineReader> {
    let reader = tokio::task::spawn_blocking(move || {
        let input = File::open(path.as_path())?;
        let decoder = zstd::stream::read::Decoder::new(input)?;
        Ok::<_, io::Error>(io::BufReader::new(Box::new(decoder) as Box<dyn Read + Send>).lines())
    })
    .await
    .map_err(io::Error::other)??;
    Ok(RolloutLineReader {
        inner: RolloutLineReaderInner::Blocking(Some(reader)),
    })
}

#[cfg(unix)]
fn create_file_with_permissions(path: &Path, permissions: &Permissions) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(permissions.mode() & 0o7777)
        .open(path)?;
    file.set_permissions(permissions.clone())?;
    Ok(file)
}

#[cfg(not(unix))]
fn create_file_with_permissions(path: &Path, permissions: &Permissions) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.set_permissions(permissions.clone())?;
    Ok(file)
}

fn temp_path_for(path: &Path, operation: &str) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(OsStr::to_os_string)
        .unwrap_or_else(|| OsStr::new("rollout").to_os_string());
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    file_name.push(format!(
        ".{operation}.{}.{counter}{TEMP_SUFFIX}",
        std::process::id()
    ));
    path.with_file_name(file_name)
}

#[cfg(test)]
#[path = "compression_tests.rs"]
mod tests;
