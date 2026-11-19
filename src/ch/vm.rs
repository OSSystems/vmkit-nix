/// One cloud-hypervisor command line: what to boot and the devices it gets.
/// [`Vm::args`] renders it; vmkit never keeps a VM as its child, so the
/// caller decides whether to exec it, run it under systemd or supervise it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vm {
    /// The cloud-hypervisor executable.
    pub binary: String,
    /// Where the REST API listens.
    pub api_socket: String,
    /// UEFI firmware to boot from; when empty, `kernel`, `initramfs` and
    /// `cmdline` boot a kernel directly.
    pub firmware: String,
    /// The kernel to boot directly.
    pub kernel: String,
    /// The initramfs for a direct kernel boot, if any.
    pub initramfs: String,
    /// The kernel command line for a direct kernel boot, if any.
    pub cmdline: String,
    /// vCPUs, all present at boot.
    pub cpus: u32,
    /// Exposes hardware virtualization to the guest.
    pub nested: bool,
    /// Boot memory. Memory is always shared, so virtiofs works.
    pub memory_mib: i64,
    /// How much more virtio-mem may plug.
    pub hotplug_mib: i64,
    /// Raw disk images.
    pub disks: Vec<Disk>,
    /// Network devices.
    pub nets: Vec<Net>,
    /// The guest's vsock address; only with `vsock_socket`.
    pub vsock_cid: u32,
    /// The host side of the guest's vsock device; empty for none.
    pub vsock_socket: String,
    /// cloud-hypervisor's `--serial` value: `tty`, `off`, `socket=<path>`…;
    /// empty means off.
    pub serial: String,
    /// virtiofs file systems.
    pub shares: Vec<Share>,
}

/// A raw disk image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Disk {
    /// The image file.
    pub path: String,
    /// Attaches the image read-only.
    pub read_only: bool,
    /// Opens the image with `O_DIRECT`.
    pub direct: bool,
}

/// A network device on a tap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Net {
    /// The tap, which must exist: see [`crate::tap`].
    pub tap: String,
    /// The guest's MAC address.
    pub mac: String,
}

/// A virtiofs file system the guest mounts by `tag`, served by a virtiofsd
/// listening at `socket`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Share {
    /// The tag the guest mounts.
    pub tag: String,
    /// virtiofsd's socket.
    pub socket: String,
}

impl Vm {
    /// The command line, `binary` first. Free page reporting is always on:
    /// memory the guest frees goes back to the host.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let mut argv = vec![self.binary.clone(), "--api-socket".into(), format!("path={}", self.api_socket)];
        if self.firmware.is_empty() {
            argv.extend(["--kernel".into(), self.kernel.clone()]);
            if !self.initramfs.is_empty() {
                argv.extend(["--initramfs".into(), self.initramfs.clone()]);
            }
            if !self.cmdline.is_empty() {
                argv.extend(["--cmdline".into(), self.cmdline.clone()]);
            }
        } else {
            argv.extend(["--firmware".into(), self.firmware.clone()]);
        }
        let nested = if self.nested { "on" } else { "off" };
        argv.extend(["--cpus".into(), format!("boot={0},max={0},nested={nested}", self.cpus)]);
        let memory = if self.hotplug_mib > 0 {
            format!("size={}M,hotplug_method=virtio-mem,hotplug_size={}M,shared=on", self.memory_mib, self.hotplug_mib)
        } else {
            format!("size={}M,shared=on", self.memory_mib)
        };
        argv.extend(["--memory".into(), memory, "--balloon".into(), "free_page_reporting=on".into()]);
        if !self.disks.is_empty() {
            argv.push("--disk".into());
            argv.extend(self.disks.iter().map(|d| {
                let mut disk = format!("path={}", d.path);
                if d.read_only {
                    disk += ",readonly=on";
                }
                if d.direct {
                    disk += ",direct=on";
                }
                disk + ",image_type=raw"
            }));
        }
        for n in &self.nets {
            argv.extend(["--net".into(), format!("tap={},mac={}", n.tap, n.mac)]);
        }
        if !self.vsock_socket.is_empty() {
            argv.extend(["--vsock".into(), format!("cid={},socket={}", self.vsock_cid, self.vsock_socket)]);
        }
        let serial = if self.serial.is_empty() { "off" } else { &self.serial };
        argv.extend(["--serial".into(), serial.into(), "--console".into(), "off".into()]);
        for s in &self.shares {
            argv.extend(["--fs".into(), format!("tag={},socket={}", s.tag, s.socket)]);
        }
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_boot_args() {
        let vm = Vm {
            binary: "/bin/ch".into(),
            api_socket: "/run/vm/api.sock".into(),
            kernel: "/k".into(),
            initramfs: "/i".into(),
            cmdline: "console=ttyS0".into(),
            cpus: 4,
            memory_mib: 2048,
            hotplug_mib: 14336,
            disks: vec![Disk { path: "/run/vm/disk.img".into(), direct: true, ..Disk::default() }],
            nets: vec![Net { tap: "tap0".into(), mac: "02:00:00:00:00:01".into() }],
            vsock_cid: 5,
            vsock_socket: "/run/vm/vsock.sock".into(),
            serial: "tty".into(),
            shares: vec![Share { tag: "nix-store".into(), socket: "/run/vm/fs-nix-store.sock".into() }],
            ..Vm::default()
        };
        assert_eq!(
            vm.args(),
            [
                "/bin/ch",
                "--api-socket",
                "path=/run/vm/api.sock",
                "--kernel",
                "/k",
                "--initramfs",
                "/i",
                "--cmdline",
                "console=ttyS0",
                "--cpus",
                "boot=4,max=4,nested=off",
                "--memory",
                "size=2048M,hotplug_method=virtio-mem,hotplug_size=14336M,shared=on",
                "--balloon",
                "free_page_reporting=on",
                "--disk",
                "path=/run/vm/disk.img,direct=on,image_type=raw",
                "--net",
                "tap=tap0,mac=02:00:00:00:00:01",
                "--vsock",
                "cid=5,socket=/run/vm/vsock.sock",
                "--serial",
                "tty",
                "--console",
                "off",
                "--fs",
                "tag=nix-store,socket=/run/vm/fs-nix-store.sock",
            ]
        );
    }

    #[test]
    fn firmware_boot_args() {
        let vm = Vm {
            binary: "/bin/ch".into(),
            api_socket: "/a".into(),
            firmware: "/fw".into(),
            kernel: "/ignored".into(),
            cpus: 2,
            nested: true,
            memory_mib: 1024,
            disks: vec![
                Disk { path: "/disk".into(), ..Disk::default() },
                Disk { path: "/seed".into(), read_only: true, ..Disk::default() },
            ],
            ..Vm::default()
        };
        assert_eq!(
            vm.args(),
            [
                "/bin/ch",
                "--api-socket",
                "path=/a",
                "--firmware",
                "/fw",
                "--cpus",
                "boot=2,max=2,nested=on",
                "--memory",
                "size=1024M,shared=on",
                "--balloon",
                "free_page_reporting=on",
                "--disk",
                "path=/disk,image_type=raw",
                "path=/seed,readonly=on,image_type=raw",
                "--serial",
                "off",
                "--console",
                "off",
            ]
        );
    }
}
