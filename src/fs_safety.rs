use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Component;

const SAFE_DIRECTORY_MODE: libc::mode_t = 0o755;
const PRIVATE_FILE_MODE: libc::mode_t = 0o600;

pub fn validate_absolute_normalized(path: &Utf8Path, label: &str) -> Result<()> {
    if !path.is_absolute() {
        bail!("{label} must be an absolute path: {path}");
    }
    if path
        .as_str()
        .split('/')
        .any(|component| component == "." || component == "..")
    {
        bail!("{label} must not contain '.' or '..' components: {path}");
    }
    for component in path.as_std_path().components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            bail!("{label} must not contain '.' or '..' components: {path}");
        }
    }
    Ok(())
}

pub fn read_config_text(path: &Utf8Path) -> Result<String> {
    validate_absolute_normalized(path, "config path")?;
    let (parent, name) = split_parent_name(path, "config path")?;
    let parent_fd = walk_directory(parent, false, running_as_root())?;
    let file = openat_file(
        parent_fd.as_raw_fd(),
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )
    .with_context(|| format!("failed to open config {path} without following symlinks"))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat config {path}"))?;
    require_regular_file(&metadata, path, "config")?;
    if running_as_root() {
        require_root_owned_file(&metadata, path, "config")?;
    }
    let mut text = String::new();
    (&file)
        .read_to_string(&mut text)
        .with_context(|| format!("failed to read config {path}"))?;
    Ok(text)
}

pub fn prepare_state_database(path: &Utf8Path) -> Result<()> {
    validate_absolute_normalized(path, "state database path")?;
    let (parent, name) = split_parent_name(path, "state database path")?;
    let parent_fd = walk_directory(parent, true, running_as_root())?;
    let file = openat_file(
        parent_fd.as_raw_fd(),
        name,
        libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        PRIVATE_FILE_MODE,
    )
    .with_context(|| format!("failed to open state database {path} safely"))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat state database {path}"))?;
    require_regular_file(&metadata, path, "state database")?;
    if running_as_root() {
        require_root_owned_file(&metadata, path, "state database")?;
        file.set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))
            .with_context(|| format!("failed to chmod state database {path}"))?;
    }
    Ok(())
}

pub fn ensure_directory_nofollow(path: &Utf8Path) -> Result<OwnedFd> {
    validate_absolute_normalized(path, "directory path")?;
    walk_directory(path, true, false)
}

pub fn open_directory_nofollow(path: &Utf8Path) -> Result<OwnedFd> {
    validate_absolute_normalized(path, "directory path")?;
    walk_directory(path, false, false)
}

pub fn open_file_at_nofollow(directory: RawFd, name: &str) -> Result<File> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        bail!("invalid file name: {name:?}");
    }
    openat_file(
        directory,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )
}

pub fn state_parent(path: &Utf8Path) -> Result<Utf8PathBuf> {
    path.parent()
        .filter(|parent| !parent.as_str().is_empty())
        .map(Utf8Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("state database path has no parent: {path}"))
}

fn walk_directory(path: &Utf8Path, create: bool, require_root_trusted: bool) -> Result<OwnedFd> {
    let root = CString::new("/")?;
    // SAFETY: root is a valid NUL-terminated path and open returns a new fd.
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(io::Error::last_os_error()).context("failed to open filesystem root");
    }
    // SAFETY: open returned a newly owned descriptor.
    let mut current = unsafe { OwnedFd::from_raw_fd(root_fd) };
    if require_root_trusted {
        validate_trusted_directory(&metadata_for_fd(&current)?, Utf8Path::new("/"))?;
    }

    let mut traversed = Utf8PathBuf::from("/");
    for component in path.as_std_path().components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("path component is not UTF-8 in {path}"))?;
        traversed.push(name);
        let child = match openat_directory(current.as_raw_fd(), name) {
            Ok(fd) => fd,
            Err(error) if create && error.raw_os_error() == Some(libc::ENOENT) => {
                mkdirat(current.as_raw_fd(), name)?;
                openat_directory(current.as_raw_fd(), name)?
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to open {traversed} without symlinks"));
            }
        };
        if require_root_trusted {
            validate_trusted_directory(&metadata_for_fd(&child)?, &traversed)?;
        }
        current = child;
    }
    Ok(current)
}

fn openat_directory(parent: RawFd, name: &str) -> io::Result<OwnedFd> {
    let name = cstring(name)?;
    // SAFETY: parent is an open directory and name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a newly owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn openat_file(parent: RawFd, name: &str, flags: libc::c_int, mode: libc::mode_t) -> Result<File> {
    let name = cstring(name)?;
    // SAFETY: parent is an open directory and name is NUL-terminated.
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags, mode) };
    if fd < 0 {
        return Err(io::Error::last_os_error()).with_context(|| format!("failed to open {name:?}"));
    }
    // SAFETY: openat returned a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn mkdirat(parent: RawFd, name: &str) -> Result<()> {
    let name = cstring(name)?;
    // SAFETY: parent is an open directory and name is NUL-terminated.
    let result = unsafe { libc::mkdirat(parent, name.as_ptr(), SAFE_DIRECTORY_MODE) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EEXIST) {
            return Err(error).with_context(|| format!("failed to create directory {name:?}"));
        }
    }
    Ok(())
}

fn metadata_for_fd(fd: &OwnedFd) -> Result<Metadata> {
    let file = File::from(fd.try_clone()?);
    file.metadata()
        .context("failed to stat directory descriptor")
}

fn validate_trusted_directory(metadata: &Metadata, path: &Utf8Path) -> Result<()> {
    if !metadata.is_dir() {
        bail!("trusted path component is not a directory: {path}");
    }
    if metadata.uid() != 0 {
        bail!("trusted path component is not root-owned: {path}");
    }
    let mode = metadata.mode();
    let writable_by_others = mode & 0o022 != 0;
    let sticky_root_directory = metadata.uid() == 0 && mode & libc::S_ISVTX != 0;
    if writable_by_others && !sticky_root_directory {
        bail!("trusted path component is group/other-writable: {path}");
    }
    Ok(())
}

fn require_regular_file(metadata: &Metadata, path: &Utf8Path, label: &str) -> Result<()> {
    if !metadata.file_type().is_file() {
        bail!("{label} is not a regular file: {path}");
    }
    Ok(())
}

fn require_root_owned_file(metadata: &Metadata, path: &Utf8Path, label: &str) -> Result<()> {
    if metadata.uid() != 0 {
        bail!("{label} must be root-owned when aur-step runs as root: {path}");
    }
    if metadata.mode() & 0o022 != 0 {
        bail!("{label} must not be group/other-writable: {path}");
    }
    Ok(())
}

fn split_parent_name<'a>(path: &'a Utf8Path, label: &str) -> Result<(&'a Utf8Path, &'a str)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{label} has no parent: {path}"))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{label} has no file name: {path}"))?;
    Ok((parent, name))
}

fn cstring(value: &str) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn running_as_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_and_parent_paths() {
        assert!(validate_absolute_normalized(Utf8Path::new("relative"), "test").is_err());
        assert!(validate_absolute_normalized(Utf8Path::new("/tmp/./file"), "test").is_err());
        assert!(validate_absolute_normalized(Utf8Path::new("/tmp/../etc"), "test").is_err());
    }

    #[test]
    fn nofollow_directory_walk_rejects_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let link = Utf8PathBuf::from_path_buf(link).unwrap();
        assert!(open_directory_nofollow(&link).is_err());
    }
}
