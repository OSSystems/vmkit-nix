package ch

import (
	"fmt"
	"strconv"
)

// VM is one cloud-hypervisor command line: what to boot and the devices it
// gets. Args renders it; vmkit never keeps a VM as its child, so the caller
// decides whether to exec it, run it under systemd or supervise it.
type VM struct {
	// Binary is the cloud-hypervisor executable.
	Binary    string
	APISocket string
	// Firmware boots from UEFI firmware; otherwise Kernel, Initramfs and
	// Cmdline boot a kernel directly.
	Firmware  string
	Kernel    string
	Initramfs string
	Cmdline   string
	CPUs      int
	// Nested exposes hardware virtualization to the guest.
	Nested bool
	// MemoryMiB is the boot memory; HotplugMiB more can be plugged with
	// virtio-mem. Memory is always shared, so virtiofs works.
	MemoryMiB  int64
	HotplugMiB int64
	Disks      []Disk
	Nets       []Net
	// VsockCID and VsockSocket give the guest a vsock device when set.
	VsockCID    uint32
	VsockSocket string
	// Serial is cloud-hypervisor's --serial value: "tty", "off",
	// "socket=<path>"...; empty means off.
	Serial string
	Shares []Share
}

type Disk struct {
	Path     string
	ReadOnly bool
	// Direct opens the image with O_DIRECT.
	Direct bool
}

type Net struct {
	Tap string
	MAC string
}

// Share is a virtiofs filesystem the guest mounts by Tag, served by a
// virtiofsd listening at Socket.
type Share struct {
	Tag    string
	Socket string
}

// Args is the command line, Binary first. Free page reporting is always on:
// memory the guest frees goes back to the host.
func (vm VM) Args() []string {
	argv := []string{vm.Binary, "--api-socket", "path=" + vm.APISocket}
	if vm.Firmware != "" {
		argv = append(argv, "--firmware", vm.Firmware)
	} else {
		argv = append(argv, "--kernel", vm.Kernel)
		if vm.Initramfs != "" {
			argv = append(argv, "--initramfs", vm.Initramfs)
		}
		if vm.Cmdline != "" {
			argv = append(argv, "--cmdline", vm.Cmdline)
		}
	}
	argv = append(argv, "--cpus", fmt.Sprintf("boot=%d,max=%d,nested=%s", vm.CPUs, vm.CPUs, onOff(vm.Nested)))
	memory := fmt.Sprintf("size=%dM,shared=on", vm.MemoryMiB)
	if vm.HotplugMiB > 0 {
		memory = fmt.Sprintf("size=%dM,hotplug_method=virtio-mem,hotplug_size=%dM,shared=on", vm.MemoryMiB, vm.HotplugMiB)
	}
	argv = append(argv, "--memory", memory, "--balloon", "free_page_reporting=on")
	if len(vm.Disks) > 0 {
		argv = append(argv, "--disk")
		for _, d := range vm.Disks {
			disk := "path=" + d.Path
			if d.ReadOnly {
				disk += ",readonly=on"
			}
			if d.Direct {
				disk += ",direct=on"
			}
			argv = append(argv, disk+",image_type=raw")
		}
	}
	for _, n := range vm.Nets {
		argv = append(argv, "--net", "tap="+n.Tap+",mac="+n.MAC)
	}
	if vm.VsockSocket != "" {
		argv = append(argv, "--vsock", "cid="+strconv.FormatUint(uint64(vm.VsockCID), 10)+",socket="+vm.VsockSocket)
	}
	serial := vm.Serial
	if serial == "" {
		serial = "off"
	}
	argv = append(argv, "--serial", serial, "--console", "off")
	for _, s := range vm.Shares {
		argv = append(argv, "--fs", "tag="+s.Tag+",socket="+s.Socket)
	}
	return argv
}

func onOff(b bool) string {
	if b {
		return "on"
	}
	return "off"
}
