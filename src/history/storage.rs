use anyhow::{Result, bail};
use rusqlite::{Connection, OpenFlags, backup::Backup};
use std::{
    fs,
    io::Write,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DB_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn open_database(path: &Path, readonly: bool) -> Result<Connection> {
    let flags = if readonly {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let connection = Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(DB_BUSY_TIMEOUT)?;
    Ok(connection)
}

pub(super) fn copy_database(source: &Path, destination: &Path) -> Result<()> {
    let source_connection = open_database(source, true)?;
    let mut destination_connection = Connection::open(destination)?;
    destination_connection.busy_timeout(DB_BUSY_TIMEOUT)?;
    let backup = Backup::new(&source_connection, &mut destination_connection)?;
    backup.run_to_completion(128, Duration::from_millis(25), None)?;
    Ok(())
}

pub(super) fn atomic_copy(source: &Path, destination: &Path) -> Result<()> {
    atomic_write(destination, &fs::read(source)?)
}

pub(super) fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        bail!("拒绝原子替换符号链接：{}", path.display());
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("history");
    let temporary = path.with_file_name(format!(".{file_name}.codex-sync-{nonce}.tmp"));
    let result = (|| -> Result<()> {
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        output.write_all(content)?;
        output.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
