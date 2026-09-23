use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub const MAX_STATE_BYTES: usize = 1024 * 1024;

pub fn atomic_write_json<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    secret: bool,
) -> Result<(), StateError> {
    let parent = path.parent().ok_or(StateError::MissingParent)?;
    create_private_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        uuid::Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if secret { 0o600 } else { 0o644 });
    }
    let result = write_and_replace(&temporary, path, parent, value, options);

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }

    result
}

pub fn atomic_write_bytes(path: &Path, contents: &[u8], secret: bool) -> Result<(), StateError> {
    let parent = path.parent().ok_or(StateError::MissingParent)?;
    create_private_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        uuid::Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if secret { 0o600 } else { 0o644 });
    }

    let result = (|| -> Result<(), StateError> {
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_directory(parent)?;

        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }

    result
}

pub fn create_private_dir_all(path: &Path) -> Result<(), StateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }

    #[cfg(not(unix))]
    fs::create_dir_all(path)?;

    Ok(())
}

fn write_and_replace<T: Serialize + ?Sized>(
    temporary: &Path,
    path: &Path,
    parent: &Path,
    value: &T,
    options: OpenOptions,
) -> Result<(), StateError> {
    let mut file = options.open(temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    sync_directory(parent)?;

    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, StateError> {
    Ok(serde_json::from_slice(&read_limited(
        path,
        MAX_STATE_BYTES,
    )?)?)
}

pub fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>, StateError> {
    let file = fs::File::open(path)?;
    let metadata = file.metadata()?;

    if !metadata.is_file() {
        return Err(StateError::NotARegularFile);
    }

    if metadata.len() > limit as u64 {
        return Err(StateError::TooLarge { limit });
    }

    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take(limit as u64 + 1).read_to_end(&mut contents)?;

    if contents.len() > limit {
        return Err(StateError::TooLarge { limit });
    }

    Ok(contents)
}

pub fn read_limited_to_string(path: &Path, limit: usize) -> Result<String, StateError> {
    String::from_utf8(read_limited(path, limit)?).map_err(|_| StateError::NotUtf8)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), StateError> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), StateError> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("state path has no parent directory")]
    MissingParent,
    #[error("state path is not a regular file")]
    NotARegularFile,
    #[error("state file is larger than {limit} bytes")]
    TooLarge { limit: usize },
    #[error("state file is not valid UTF-8")]
    NotUtf8,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct State {
        value: String,
    }

    #[test]
    fn test_atomic_state_roundtrip() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("trust/peers.json");
        let expected = State {
            value: "trusted".to_owned(),
        };

        atomic_write_json(&path, &expected, true).expect("state written");
        let actual: State = read_json(&path).expect("state read");

        assert_eq!(actual, expected);
    }

    #[cfg(unix)]
    #[test]
    fn test_secret_state_is_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("node-key");

        atomic_write_json(&path, &"secret", true).expect("secret written");

        assert_eq!(
            fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_created_state_directories_are_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("access/nested/allowlist.json");

        atomic_write_json(&path, &"value", false).expect("state written");

        let mode = fs::metadata(path.parent().expect("parent"))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn test_reading_rejects_oversized_state() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("large.json");
        fs::write(&path, vec![b'x'; 64]).expect("file written");

        let error = read_limited(&path, 32).expect_err("oversized state is rejected");

        assert!(matches!(error, StateError::TooLarge { limit: 32 }));
    }

    #[test]
    fn test_reading_rejects_a_directory_in_place_of_state() {
        let directory = tempfile::tempdir().expect("temporary directory");

        let error = read_limited(directory.path(), 32).expect_err("a directory is not state");

        assert!(matches!(
            error,
            StateError::NotARegularFile | StateError::Io(_)
        ));
    }

    #[test]
    fn test_atomic_bytes_replace_existing_content() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");

        atomic_write_bytes(&path, b"first", false).expect("first write");
        atomic_write_bytes(&path, b"second", false).expect("second write");

        assert_eq!(
            read_limited_to_string(&path, MAX_STATE_BYTES).expect("read"),
            "second"
        );
    }
}
