//! Shares host directories with guests through virtiofsd, and builds a
//! store holding only one Nix closure, so a guest sees its own system and
//! nothing else of the host's store.

use std::ffi::CString;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::hostutil;

/// The largest closure listing read.
const MAX_STORE_PATHS: u64 = 16 << 20;

/// virtiofsd's arguments to serve `dir` read-only at `socket`.
#[must_use]
pub fn args(socket: &str, dir: &str) -> Vec<String> {
    vec![format!("--socket-path={socket}"), format!("--shared-dir={dir}"), "--readonly".into(), "--cache=auto".into()]
}

/// A closure's store paths, one per line, as `closureInfo`'s `store-paths`
/// file lists them. Anything but a top-level `/nix/store` entry is refused.
pub fn store_paths(closure: impl AsRef<Path>) -> io::Result<Vec<String>> {
    let raw = hostutil::read_bounded(closure, MAX_STORE_PATHS)?;
    let text =
        String::from_utf8(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "store paths are not UTF-8"))?;
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| match line.strip_prefix("/nix/store/") {
            Some(name) if !name.is_empty() && name != "." && name != ".." && !name.contains('/') => Ok(line.to_owned()),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, format!("not a store path: {line:?}"))),
        })
        .collect()
}

/// Makes `dir` a store holding only the closure: each path is bind-mounted
/// read-only from the host store. The mounts belong to the calling
/// process's mount namespace; [`require_private_mounts`] first keeps them
/// out of the host's.
pub fn store_view(closure: impl AsRef<Path>, dir: impl AsRef<Path>) -> io::Result<()> {
    let dir = dir.as_ref();
    let paths = store_paths(closure)?;
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    for path in paths {
        let source = Path::new(&path);
        let Some(name) = source.file_name() else { continue };
        let target = dir.join(name);
        let meta = fs::symlink_metadata(source)?;
        if meta.file_type().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(source)?, &target)?;
            continue;
        }
        if meta.is_dir() {
            DirBuilder::new().mode(0o555).create(&target)?;
        } else {
            fs::OpenOptions::new().write(true).create_new(true).mode(0o444).open(&target)?;
        }
        mount(source, &target, libc::MS_BIND).map_err(|e| io::Error::new(e.kind(), format!("binding {path}: {e}")))?;
        mount(
            Path::new(""),
            &target,
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV,
        )
        .map_err(|e| io::Error::new(e.kind(), format!("making {path} read-only: {e}")))?;
    }
    Ok(())
}

fn mount(source: &Path, target: &Path, flags: libc::c_ulong) -> io::Result<()> {
    let c = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput));
    let (source, target) = (c(source)?, c(target)?);
    // SAFETY: both paths are NUL-terminated and outlive the call; a bind
    // mount takes no file system type or data.
    let rc = unsafe { libc::mount(source.as_ptr(), target.as_ptr(), std::ptr::null(), flags, std::ptr::null()) };
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Fails unless the process runs in a mount namespace of its own
/// (systemd's `PrivateMounts=`), so [`store_view`]'s mounts go with it.
pub fn require_private_mounts() -> io::Result<()> {
    if fs::read_link("/proc/self/ns/mnt")? == fs::read_link("/proc/1/ns/mnt")? {
        return Err(io::Error::other("run in a private mount namespace (PrivateMounts=yes)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_store_paths() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("store-paths");
        fs::write(&file, "/nix/store/aaa-t3code\n/nix/store/bbb-glibc\n\n").unwrap();
        assert_eq!(store_paths(&file).unwrap(), ["/nix/store/aaa-t3code", "/nix/store/bbb-glibc"]);
        for bad in ["/etc/shadow\n", "/nix/store/aaa/../../etc\n", "/nix/store/..\n", "/nix/store/\n", "/nix/store/.\n"]
        {
            fs::write(&file, bad).unwrap();
            assert!(store_paths(&file).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn virtiofsd_args() {
        assert_eq!(args("/s", "/d"), ["--socket-path=/s", "--shared-dir=/d", "--readonly", "--cache=auto"]);
    }
}
