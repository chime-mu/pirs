//! `fs.list` and `fs.read`: read-only file access on the server that runs
//! the loop (D-07, D-39).
//!
//! Paths are used as given; a relative one is resolved against the loop's
//! cwd. The server never interprets a client's intent beyond that: it reads
//! whatever its user can read, a `ref` from a by-reference payload included.
//! `fs.read` serves any readable file up to [`FS_READ_CAP`] in full and
//! refuses larger ones with `INVALID_PARAMS`; a missing path is `NOT_FOUND`.

use std::path::{Path, PathBuf};

use pirs_protocol::{code, FsEntry, FsEntryKind, FsListResult, FsReadResult, RpcError, ServerPath};

use crate::FS_READ_CAP;

/// Join a request path onto the loop's cwd and normalise it lexically
/// (`.` and `..` components), so the paths the server hands out are the
/// same spelling whatever the client wrote. Symlinks are not resolved.
fn resolve(cwd: &Path, path: &ServerPath) -> PathBuf {
    let p = Path::new(path.as_str());
    let joined = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn io_error(path: &Path, e: std::io::Error) -> RpcError {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => code::NOT_FOUND,
        _ => code::INTERNAL_ERROR,
    };
    RpcError::new(code, format!("{}: {e}", path.display()))
}

/// List a directory, entries sorted by name.
pub(crate) async fn list(cwd: &Path, path: &ServerPath) -> Result<FsListResult, RpcError> {
    let dir = resolve(cwd, path);
    let mut read = tokio::fs::read_dir(&dir).await.map_err(|e| io_error(&dir, e))?;
    let mut entries = Vec::new();
    while let Some(entry) = read.next_entry().await.map_err(|e| io_error(&dir, e))? {
        let full = entry.path();
        // Follow symlinks so a link to a directory lists as one.
        let Ok(meta) = tokio::fs::metadata(&full).await else { continue };
        let kind = if meta.is_dir() { FsEntryKind::Dir } else { FsEntryKind::File };
        entries.push(FsEntry {
            path: ServerPath::from(full.to_string_lossy().into_owned()),
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            bytes: (kind == FsEntryKind::File).then_some(meta.len()),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(FsListResult { entries })
}

/// Read a file in full (lossy UTF-8), or a `ref`.
pub(crate) async fn read(cwd: &Path, path: &ServerPath) -> Result<FsReadResult, RpcError> {
    let file = resolve(cwd, path);
    let meta = tokio::fs::metadata(&file).await.map_err(|e| io_error(&file, e))?;
    if meta.is_dir() {
        return Err(RpcError::new(code::INVALID_PARAMS, format!("{} is a directory", file.display())));
    }
    if meta.len() > FS_READ_CAP {
        return Err(RpcError::new(
            code::INVALID_PARAMS,
            format!("{} is {} bytes, above the {FS_READ_CAP} byte cap", file.display(), meta.len()),
        ));
    }
    let bytes = tokio::fs::read(&file).await.map_err(|e| io_error(&file, e))?;
    Ok(FsReadResult::Content { content: String::from_utf8_lossy(&bytes).into_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lists_and_reads_relative_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let listed = list(dir.path(), &ServerPath::from(".")).await.unwrap();
        let names: Vec<(&str, FsEntryKind, Option<u64>)> =
            listed.entries.iter().map(|e| (e.name.as_str(), e.kind, e.bytes)).collect();
        assert_eq!(names, [("a.txt", FsEntryKind::File, Some(5)), ("sub", FsEntryKind::Dir, None)]);
        assert!(listed.entries[0].path.as_str().starts_with(dir.path().to_str().unwrap()));

        let read_back = read(dir.path(), &ServerPath::from("a.txt")).await.unwrap();
        assert!(matches!(read_back, FsReadResult::Content { content } if content == "hello"));

        let missing = read(dir.path(), &ServerPath::from("nope")).await.unwrap_err();
        assert_eq!(missing.code, code::NOT_FOUND);
        let is_dir = read(dir.path(), &ServerPath::from("sub")).await.unwrap_err();
        assert_eq!(is_dir.code, code::INVALID_PARAMS);
    }
}
