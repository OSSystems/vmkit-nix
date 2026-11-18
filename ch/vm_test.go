package ch

import (
	"slices"
	"testing"
)

func TestKernelBootArgs(t *testing.T) {
	vm := VM{
		Binary: "/bin/ch", APISocket: "/run/vm/api.sock",
		Kernel: "/k", Initramfs: "/i", Cmdline: "console=ttyS0",
		CPUs: 4, MemoryMiB: 2048, HotplugMiB: 14336,
		Disks:    []Disk{{Path: "/run/vm/disk.img", Direct: true}},
		Nets:     []Net{{Tap: "tap0", MAC: "02:00:00:00:00:01"}},
		VsockCID: 5, VsockSocket: "/run/vm/vsock.sock",
		Serial: "tty",
		Shares: []Share{{Tag: "nix-store", Socket: "/run/vm/fs-nix-store.sock"}},
	}
	want := []string{"/bin/ch",
		"--api-socket", "path=/run/vm/api.sock",
		"--kernel", "/k", "--initramfs", "/i", "--cmdline", "console=ttyS0",
		"--cpus", "boot=4,max=4,nested=off",
		"--memory", "size=2048M,hotplug_method=virtio-mem,hotplug_size=14336M,shared=on",
		"--balloon", "free_page_reporting=on",
		"--disk", "path=/run/vm/disk.img,direct=on,image_type=raw",
		"--net", "tap=tap0,mac=02:00:00:00:00:01",
		"--vsock", "cid=5,socket=/run/vm/vsock.sock",
		"--serial", "tty", "--console", "off",
		"--fs", "tag=nix-store,socket=/run/vm/fs-nix-store.sock",
	}
	if got := vm.Args(); !slices.Equal(got, want) {
		t.Fatalf("args =\n%q\nwant\n%q", got, want)
	}
}

func TestFirmwareBootArgs(t *testing.T) {
	vm := VM{
		Binary: "/bin/ch", APISocket: "/a", Firmware: "/fw", Kernel: "/ignored",
		CPUs: 2, Nested: true, MemoryMiB: 1024,
		Disks: []Disk{{Path: "/disk"}, {Path: "/seed", ReadOnly: true}},
	}
	want := []string{"/bin/ch",
		"--api-socket", "path=/a",
		"--firmware", "/fw",
		"--cpus", "boot=2,max=2,nested=on",
		"--memory", "size=1024M,shared=on",
		"--balloon", "free_page_reporting=on",
		"--disk", "path=/disk,image_type=raw", "path=/seed,readonly=on,image_type=raw",
		"--serial", "off", "--console", "off",
	}
	if got := vm.Args(); !slices.Equal(got, want) {
		t.Fatalf("args =\n%q\nwant\n%q", got, want)
	}
}

