//! The host probes and file helpers vmkit's controllers share: accounts,
//! systemd units, load, meminfo and atomic writes.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A user and the groups it runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The user id.
    pub uid: u32,
    /// The primary group id.
    pub gid: u32,
    /// Every group the user is in, the primary one included.
    pub groups: Vec<u32>,
}

/// Resolves a user name through the system's user database.
pub fn lookup_account(name: &str) -> io::Result<Account> {
    let cname = CString::new(name).map_err(|_| invalid(format!("invalid user name {name:?}")))?;
    let mut buf = vec![0u8; 4096];
    // SAFETY: an all-zero passwd is a valid value for getpwnam_r to fill.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    loop {
        // SAFETY: every pointer is valid for the call and buf's length is
        // what is passed.
        let rc = unsafe {
            libc::getpwnam_r(cname.as_ptr(), &raw mut pwd, buf.as_mut_ptr().cast(), buf.len(), &raw mut found)
        };
        match rc {
            0 => break,
            libc::ERANGE if buf.len() < 1 << 20 => buf.resize(buf.len() * 2, 0),
            rc => return Err(io::Error::from_raw_os_error(rc)),
        }
    }
    if found.is_null() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("unknown user {name:?}")));
    }
    let (uid, gid) = (pwd.pw_uid, pwd.pw_gid);
    // SAFETY: getpwnam_r succeeded, so pw_name points into buf.
    let user = unsafe { CStr::from_ptr(pwd.pw_name) }.to_owned();

    let mut groups: Vec<libc::gid_t> = vec![0; 64];
    loop {
        let mut n = libc::c_int::try_from(groups.len()).unwrap_or(libc::c_int::MAX);
        // SAFETY: groups holds n entries; getgrouplist writes at most n and
        // sets n to how many there are.
        let rc = unsafe { libc::getgrouplist(user.as_ptr(), gid, groups.as_mut_ptr(), &raw mut n) };
        let n = usize::try_from(n).unwrap_or(0);
        if rc >= 0 {
            groups.truncate(n);
            break;
        }
        if groups.len() >= 1 << 16 {
            return Err(invalid(format!("user {name:?} is in too many groups")));
        }
        groups.resize(n.max(groups.len() * 2), 0);
    }
    Ok(Account { uid, gid, groups })
}

impl Account {
    /// Makes `cmd` run as the account, killed with SIGTERM when the thread
    /// that spawns it exits (`PR_SET_PDEATHSIG` follows the thread, not the
    /// process): spawn from a thread that lives as long as the child should,
    /// never from `spawn_blocking`. With tokio, pass `cmd.as_std_mut()`.
    pub fn run_as<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        let Account { uid, gid, groups } = self.clone();
        let parent = libc::pid_t::try_from(std::process::id()).unwrap_or(-1);
        // SAFETY: the closure only makes async-signal-safe system calls and
        // allocates nothing.
        unsafe {
            cmd.pre_exec(move || {
                // The parent-death signal is reset when credentials change,
                // so it is set after them.
                check(libc::setgroups(groups.len(), groups.as_ptr()))?;
                check(libc::setgid(gid))?;
                check(libc::setuid(uid))?;
                check(libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM))?;
                // The parent may have died before the signal was armed.
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            })
        }
    }
}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Reports whether a systemd unit is up or has a start job queued. Callers
/// that must not start something twice treat an error as running.
pub async fn unit_running(unit: &str) -> io::Result<bool> {
    let show = |property: &'static str| {
        let mut cmd = tokio::process::Command::new("systemctl");
        cmd.args(["show", &format!("--property={property}"), "--value", unit]).kill_on_drop(true);
        async move {
            let out = cmd.output().await?;
            if !out.status.success() {
                return Err(io::Error::other(format!("systemctl show {unit}: {}", out.status)));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        }
    };
    let probe = async {
        if matches!(show("ActiveState").await?.as_str(), "active" | "activating" | "deactivating") {
            return Ok(true);
        }
        let job = show("Job").await?;
        Ok(!job.is_empty() && job != "0" && job != "[0 /]")
    };
    tokio::time::timeout(Duration::from_secs(3), probe)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("systemctl show {unit} timed out")))?
}

/// The 1-minute load average.
pub fn load1() -> io::Result<f64> {
    let raw = fs::read_to_string("/proc/loadavg")?;
    raw.split_whitespace()
        .next()
        .ok_or_else(|| invalid("empty /proc/loadavg".into()))?
        .parse()
        .map_err(|e| invalid(format!("/proc/loadavg: {e}")))
}

/// The fields of `/proc/meminfo`, in KiB.
pub fn meminfo_kib() -> io::Result<HashMap<String, i64>> {
    Ok(parse_meminfo(&fs::read_to_string("/proc/meminfo")?))
}

/// The fields of a `/proc/meminfo` text, in KiB; lines it cannot read are
/// left out.
#[must_use]
pub fn parse_meminfo(text: &str) -> HashMap<String, i64> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let key = fields.next()?.strip_suffix(':')?;
            let kib = fields.next()?.parse().ok()?;
            Some((key.to_owned(), kib))
        })
        .collect()
}

/// Reads a file of at most `limit` bytes.
pub fn read_bounded(path: impl AsRef<Path>, limit: u64) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(invalid(format!("file exceeds {limit} bytes")));
    }
    Ok(data)
}

/// Replaces `path` with `data`, durably: a temporary file in the same
/// directory is written, synced and renamed over it, then the directory is
/// synced.
pub fn atomic_write(path: impl AsRef<Path>, data: &[u8], mode: u32) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = path.as_ref();
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let name = path.file_name().ok_or_else(|| invalid(format!("{} has no file name", path.display())))?;
    let tmp = dir.join(format!(
        ".{}-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
        // The mode given to open is masked by the umask.
        f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Waits for a Unix socket to appear at `path`.
pub async fn wait_for_socket(path: impl AsRef<Path>, timeout: Duration) -> io::Result<()> {
    let path = path.as_ref();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if fs::metadata(path).is_ok_and(|m| m.file_type().is_socket()) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{} did not appear", path.display())));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn atomic_write_and_read_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        atomic_write(&path, b"one", 0o640).unwrap();
        atomic_write(&path, b"two", 0o640).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
        assert_eq!(read_bounded(&path, 3).unwrap(), b"two");
        assert!(read_bounded(&path, 2).is_err(), "read past the limit");
        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "leftover temporary files");
    }

    #[test]
    fn atomic_write_leaves_nothing_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("d");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("x"), b"").unwrap();
        // Renaming a file over a non-empty directory fails.
        assert!(atomic_write(&target, b"data", 0o644).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn meminfo() {
        let mem = parse_meminfo(
            "MemTotal:        8388608 kB\nMemAvailable:     812345 kB\nHugePages_Total:       0\nbroken\n",
        );
        assert_eq!(mem["MemTotal"], 8_388_608);
        assert_eq!(mem["MemAvailable"], 812_345);
        assert_eq!(mem["HugePages_Total"], 0);
        assert_eq!(mem.len(), 3);
    }

    #[test]
    fn lookup_root() {
        let root = lookup_account("root").unwrap();
        assert_eq!((root.uid, root.gid), (0, 0));
        assert!(root.groups.contains(&0));
        assert_eq!(lookup_account("no-such-user-vmkit").unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(lookup_account("bad\0name").is_err());
    }

    #[tokio::test]
    async fn waits_for_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let err = wait_for_socket(&path, Duration::from_millis(50)).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        let _listener = tokio::net::UnixListener::bind(&path).unwrap();
        wait_for_socket(&path, Duration::from_secs(1)).await.unwrap();
    }
}
