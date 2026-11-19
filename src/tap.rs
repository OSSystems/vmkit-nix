//! Persistent tap devices an unprivileged cloud-hypervisor can open.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

/// `struct ifreq` from `<linux/if.h>`: a name and a union, of which vmkit
/// uses the flags and the interface index.
#[repr(C)]
struct IfReq {
    name: [u8; libc::IFNAMSIZ],
    data: [u8; 24],
}

impl IfReq {
    fn new(name: &str) -> io::Result<Self> {
        if name.is_empty()
            || name.len() >= libc::IFNAMSIZ
            || name.bytes().any(|b| b == 0 || b == b'/' || b.is_ascii_whitespace())
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid interface name {name:?}")));
        }
        let mut r = IfReq { name: [0; libc::IFNAMSIZ], data: [0; 24] };
        r.name[..name.len()].copy_from_slice(name.as_bytes());
        Ok(r)
    }

    fn flags(&self) -> u16 {
        u16::from_ne_bytes([self.data[0], self.data[1]])
    }

    fn set_flags(&mut self, flags: u16) {
        self.data[..2].copy_from_slice(&flags.to_ne_bytes());
    }
}

// From <linux/if_tun.h> and <linux/sockios.h>.
const TUNSETIFF: libc::Ioctl = 0x4004_54ca;
const TUNSETPERSIST: libc::Ioctl = 0x4004_54cb;
const TUNSETOWNER: libc::Ioctl = 0x4004_54cc;
const SIOCBRADDIF: libc::Ioctl = 0x89a2;
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
const TAP_FLAGS: u16 = (libc::IFF_TAP | libc::IFF_NO_PI | libc::IFF_VNET_HDR) as u16;

fn ioctl_ptr(fd: &impl AsRawFd, request: libc::Ioctl, arg: &mut IfReq) -> io::Result<()> {
    // SAFETY: every request vmkit makes with a pointer takes a struct
    // ifreq, which IfReq lays out.
    if unsafe { libc::ioctl(fd.as_raw_fd(), request, std::ptr::from_mut(arg)) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn ioctl_value(fd: &impl AsRawFd, request: libc::Ioctl, value: libc::c_ulong) -> io::Result<()> {
    // SAFETY: the requests vmkit makes with a value take an integer.
    if unsafe { libc::ioctl(fd.as_raw_fd(), request, value) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn context(e: &io::Error, what: &str) -> io::Error {
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// Creates a persistent tap owned by `uid`, enslaves it to `bridge` and
/// brings it up, so an unprivileged cloud-hypervisor can attach to it
/// without `CAP_NET_ADMIN`.
pub fn create(name: &str, uid: u32, bridge: &str) -> io::Result<()> {
    let mut bridge_req = IfReq::new(bridge)?;
    let tun = open_tap(name)?;
    ioctl_value(&tun, TUNSETOWNER, libc::c_ulong::from(uid))
        .map_err(|e| context(&e, &format!("setting owner of tap {name}")))?;
    ioctl_value(&tun, TUNSETPERSIST, 1).map_err(|e| context(&e, &format!("making tap {name} persistent")))?;

    // SAFETY: a plain socket call; its result is checked before use.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a socket this function just opened and owns.
    let sock = unsafe { OwnedFd::from_raw_fd(fd) };

    let mut index = IfReq::new(name)?;
    ioctl_ptr(&sock, libc::SIOCGIFINDEX, &mut index).map_err(|e| context(&e, &format!("looking up tap {name}")))?;
    bridge_req.data[..4].copy_from_slice(&index.data[..4]);
    ioctl_ptr(&sock, SIOCBRADDIF, &mut bridge_req)
        .map_err(|e| context(&e, &format!("adding tap {name} to bridge {bridge}")))?;

    let mut up = IfReq::new(name)?;
    ioctl_ptr(&sock, libc::SIOCGIFFLAGS, &mut up)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    up.set_flags(up.flags() | libc::IFF_UP as u16);
    ioctl_ptr(&sock, libc::SIOCSIFFLAGS, &mut up).map_err(|e| context(&e, &format!("bringing tap {name} up")))
}

/// Removes a persistent tap; a missing one is not an error.
pub fn delete(name: &str) -> io::Result<()> {
    IfReq::new(name)?;
    if !Path::new("/sys/class/net").join(name).exists() {
        return Ok(());
    }
    ioctl_value(&open_tap(name)?, TUNSETPERSIST, 0).map_err(|e| context(&e, &format!("removing tap {name}")))
}

/// Attaches to the tap called `name`, creating it if missing, with the
/// flags cloud-hypervisor opens it with.
fn open_tap(name: &str) -> io::Result<File> {
    let mut req = IfReq::new(name)?;
    let tun = OpenOptions::new().read(true).write(true).open("/dev/net/tun")?;
    req.set_flags(TAP_FLAGS);
    ioctl_ptr(&tun, TUNSETIFF, &mut req).map_err(|e| context(&e, &format!("opening tap {name}")))?;
    Ok(tun)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_names() {
        for bad in ["", "sixteen-chars-xx", "a/b", "a b", "a\0b"] {
            assert_eq!(create(bad, 0, "br0").unwrap_err().kind(), io::ErrorKind::InvalidInput, "{bad:?}");
            assert_eq!(delete(bad).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        assert_eq!(create("tap0", 0, "").unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(IfReq::new("fifteen-chars-x").is_ok());
    }

    #[test]
    fn missing_tap_deletes() {
        delete("vmkit-absent0").unwrap();
    }

    #[test]
    fn ifreq_layout() {
        // struct ifreq is 40 bytes on 64-bit Linux.
        assert_eq!(std::mem::size_of::<IfReq>(), 40);
        let mut r = IfReq::new("tap0").unwrap();
        r.set_flags(TAP_FLAGS);
        assert_eq!(r.flags(), 0x5002);
    }
}
