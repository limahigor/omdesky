use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

pub fn atomic_write_json<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    secret: bool,
) -> Result<(), StateError> {
    let parent = path.parent().ok_or(StateError::MissingParent)?;
    fs::create_dir_all(parent)?;
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
    Ok(serde_json::from_reader(fs::File::open(path)?)?)
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
}
