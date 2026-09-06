use std::{
    ffi::{OsStr, OsString},
    io::{Error, ErrorKind},
    path::{Component, Path, PathBuf},
};

const DATA_DIR_OVERRIDE: &str = "IMAGE_CLIENT_DATA_DIR";

fn home_dir_from(
    user_profile: Option<PathBuf>,
    home: Option<PathBuf>,
    windows: bool,
) -> Option<PathBuf> {
    if windows {
        user_profile.or(home)
    } else {
        home.or(user_profile)
    }
}

fn platform_home_dir() -> Option<PathBuf> {
    home_dir_from(
        std::env::var_os("USERPROFILE").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        cfg!(windows),
    )
    .or_else(dirs::home_dir)
    .or_else(dirs::data_local_dir)
}

fn default_data_dir() -> PathBuf {
    let home = platform_home_dir().unwrap_or_else(|| {
        eprintln!("[paths] home directory unavailable; using temporary directory");
        std::env::temp_dir()
    });
    home.join("ImageClient")
}

fn data_dir_override() -> Option<OsString> {
    std::env::var_os(DATA_DIR_OVERRIDE)
}

fn configured_data_dir_from(
    override_value: Option<&OsStr>,
    default_dir: PathBuf,
) -> std::io::Result<PathBuf> {
    let Some(override_value) = override_value else {
        return Ok(default_dir);
    };
    let override_text = override_value.to_str().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("{DATA_DIR_OVERRIDE} must be valid Unicode"),
        )
    })?;
    if override_text.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{DATA_DIR_OVERRIDE} must not be empty"),
        ));
    }

    let directory = PathBuf::from(override_value);
    if !directory.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{DATA_DIR_OVERRIDE} must be an absolute directory path"),
        ));
    }
    if directory.parent().is_none() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{DATA_DIR_OVERRIDE} must not be a filesystem root"),
        ));
    }
    if directory
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{DATA_DIR_OVERRIDE} must not contain '..' path segments"),
        ));
    }
    match std::fs::metadata(&directory) {
        Ok(metadata) if metadata.is_dir() => {
            let canonical = directory.canonicalize().map_err(|error| {
                Error::new(
                    error.kind(),
                    format!("cannot canonicalize {DATA_DIR_OVERRIDE}: {error}"),
                )
            })?;
            if is_filesystem_root(&canonical) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("{DATA_DIR_OVERRIDE} must not resolve to a filesystem root"),
                ));
            }
            Ok(directory)
        }
        Ok(_) => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{DATA_DIR_OVERRIDE} must name a directory, not a file"),
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(directory),
        Err(error) => Err(Error::new(
            error.kind(),
            format!("cannot validate {DATA_DIR_OVERRIDE}: {error}"),
        )),
    }
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn configured_data_dir() -> std::io::Result<PathBuf> {
    let override_value = data_dir_override();
    configured_data_dir_from(override_value.as_deref(), default_data_dir())
}

/// Fixed, predictable data root: `<home>/ImageClient` — sibling to `.ssh`.
/// `IMAGE_CLIENT_DATA_DIR` may instead provide an absolute, non-root directory.
/// Windows: C:\Users\<user>\ImageClient ; macOS/Linux: ~/ImageClient
pub fn data_dir() -> PathBuf {
    configured_data_dir().unwrap_or_else(|error| {
        panic!("invalid {DATA_DIR_OVERRIDE}: {error}");
    })
}

pub fn assets_dir() -> PathBuf {
    data_dir().join("assets")
}

/// Structured application logs live beside the SQLite database.
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Cached case-library images downloaded from the public R2 host.
pub fn promptlib_images_dir() -> PathBuf {
    data_dir().join("promptlib-images")
}

/// Data dir as a forward-slash absolute path for the frontend.
pub fn data_dir_str() -> String {
    data_dir().to_string_lossy().replace('\\', "/")
}

pub fn ensure_data_dirs() -> std::io::Result<()> {
    // Validate the override before creating any directory or initializing a
    // sibling path. An invalid isolated-run override must never fall back to
    // the real user data root.
    let data_dir = configured_data_dir()?;
    for directory in [
        data_dir.clone(),
        data_dir.join("assets"),
        data_dir.join("logs"),
        data_dir.join("promptlib-images"),
    ] {
        std::fs::create_dir_all(&directory)?;
        secure_dir(&directory)?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn secure_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
pub fn secure_dir(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn secure_file(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn secure_file(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{configured_data_dir_from, home_dir_from};
    #[cfg(unix)]
    use super::{secure_dir, secure_file};
    use std::{
        ffi::{OsStr, OsString},
        path::PathBuf,
    };

    #[test]
    fn chooses_native_home_variable_first() {
        let profile = Some(PathBuf::from("C:/Users/Alice"));
        let home = Some(PathBuf::from("/home/alice"));
        assert_eq!(home_dir_from(profile.clone(), home.clone(), true), profile);
        assert_eq!(home_dir_from(profile, home.clone(), false), home);
    }

    #[test]
    fn accepts_an_absolute_non_root_override_without_an_environment_mutation() {
        let directory = std::env::temp_dir().join(format!(
            "image-client-isolated-data-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let default_dir = PathBuf::from("unused-default");
        assert_eq!(
            configured_data_dir_from(Some(directory.as_os_str()), default_dir).unwrap(),
            directory
        );
        let _ = std::fs::remove_dir(directory);
    }

    #[test]
    fn keeps_the_supplied_default_when_no_override_is_set() {
        let default_dir = std::env::temp_dir().join("image-client-default-data");
        assert_eq!(
            configured_data_dir_from(None, default_dir.clone()).unwrap(),
            default_dir
        );
    }

    #[test]
    fn rejects_empty_relative_root_parent_and_file_overrides() {
        let default_dir = std::env::temp_dir().join("unused-default");
        assert!(configured_data_dir_from(Some(OsStr::new("")), default_dir.clone()).is_err());
        assert!(configured_data_dir_from(Some(OsStr::new(" \t")), default_dir.clone()).is_err());
        assert!(
            configured_data_dir_from(Some(OsStr::new("relative-data")), default_dir.clone())
                .is_err()
        );

        let temporary_directory = std::env::temp_dir();
        let root = temporary_directory
            .ancestors()
            .last()
            .expect("temporary directory has a filesystem root");
        assert!(configured_data_dir_from(Some(root.as_os_str()), default_dir.clone()).is_err());

        let parent_directory = temporary_directory.join("isolated-data").join("..");
        assert!(
            configured_data_dir_from(Some(parent_directory.as_os_str()), default_dir.clone())
                .is_err()
        );

        let file =
            std::env::temp_dir().join(format!("image-client-paths-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&file, b"fixture").unwrap();
        assert!(configured_data_dir_from(Some(file.as_os_str()), default_dir).is_err());
        let _ = std::fs::remove_file(file);
    }

    #[cfg(windows)]
    #[test]
    fn rejects_non_unicode_windows_override() {
        use std::os::windows::ffi::OsStringExt;

        let invalid = OsString::from_wide(&[0xd800]);
        assert!(
            configured_data_dir_from(Some(invalid.as_os_str()), PathBuf::from("unused")).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_unix_override() {
        use std::os::unix::ffi::OsStringExt;

        let invalid = OsString::from_vec(vec![b'/', 0xff]);
        assert!(
            configured_data_dir_from(Some(invalid.as_os_str()), PathBuf::from("unused")).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn applies_private_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("image-client-perms-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        secure_dir(&root).unwrap();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = root.join("secret.json");
        std::fs::write(&file, b"secret").unwrap();
        secure_file(&file).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
