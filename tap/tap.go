// Package tap makes persistent tap devices an unprivileged
// cloud-hypervisor can open.
package tap

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"syscall"
	"unsafe"
)

// Linux interface ioctls, from <linux/if_tun.h> and <linux/sockios.h>.
const (
	tunSetIff     = 0x400454ca
	tunSetPersist = 0x400454cb
	tunSetOwner   = 0x400454cc
	iffTap        = 0x0002
	iffNoPI       = 0x1000
	iffVnetHdr    = 0x4000
	siocBrAddIf   = 0x89a2
)

type ifreq struct {
	name [16]byte
	data [24]byte
}

func newIfreq(name string) (*ifreq, error) {
	if len(name) == 0 || len(name) > 15 {
		return nil, fmt.Errorf("invalid interface name %q", name)
	}
	r := &ifreq{}
	copy(r.name[:], name)
	return r, nil
}

func ioctl(fd uintptr, req uintptr, arg unsafe.Pointer) error {
	if _, _, errno := syscall.Syscall(syscall.SYS_IOCTL, fd, req, uintptr(arg)); errno != 0 {
		return errno
	}
	return nil
}

func ioctlValue(fd uintptr, req uintptr, value uintptr) error {
	if _, _, errno := syscall.Syscall(syscall.SYS_IOCTL, fd, req, value); errno != 0 {
		return errno
	}
	return nil
}

// Create creates a persistent tap owned by uid, enslaves it to bridge and
// brings it up, so an unprivileged cloud-hypervisor can attach to it without
// CAP_NET_ADMIN.
func Create(name string, uid int, bridge string) error {
	tun, err := openTap(name)
	if err != nil {
		return err
	}
	defer tun.Close()
	if err := ioctlValue(tun.Fd(), tunSetOwner, uintptr(uid)); err != nil {
		return fmt.Errorf("setting owner of tap %s: %w", name, err)
	}
	if err := ioctlValue(tun.Fd(), tunSetPersist, 1); err != nil {
		return fmt.Errorf("making tap %s persistent: %w", name, err)
	}
	sock, err := syscall.Socket(syscall.AF_INET, syscall.SOCK_DGRAM|syscall.SOCK_CLOEXEC, 0)
	if err != nil {
		return err
	}
	defer syscall.Close(sock)
	index := &ifreq{}
	copy(index.name[:], name)
	if err := ioctl(uintptr(sock), syscall.SIOCGIFINDEX, unsafe.Pointer(index)); err != nil {
		return fmt.Errorf("looking up tap %s: %w", name, err)
	}
	br, err := newIfreq(bridge)
	if err != nil {
		return err
	}
	copy(br.data[:4], index.data[:4])
	if err := ioctl(uintptr(sock), siocBrAddIf, unsafe.Pointer(br)); err != nil {
		return fmt.Errorf("adding tap %s to bridge %s: %w", name, bridge, err)
	}
	up := &ifreq{}
	copy(up.name[:], name)
	if err := ioctl(uintptr(sock), syscall.SIOCGIFFLAGS, unsafe.Pointer(up)); err != nil {
		return err
	}
	*(*uint16)(unsafe.Pointer(&up.data[0])) |= syscall.IFF_UP
	if err := ioctl(uintptr(sock), syscall.SIOCSIFFLAGS, unsafe.Pointer(up)); err != nil {
		return fmt.Errorf("bringing tap %s up: %w", name, err)
	}
	return nil
}

// Delete removes a persistent tap; a missing one is not an error.
func Delete(name string) error {
	if _, err := os.Stat(filepath.Join("/sys/class/net", name)); errors.Is(err, os.ErrNotExist) {
		return nil
	}
	tun, err := openTap(name)
	if err != nil {
		return err
	}
	defer tun.Close()
	return ioctlValue(tun.Fd(), tunSetPersist, 0)
}

// openTap attaches to the tap called name, creating it if missing, with the
// flags cloud-hypervisor opens it with.
func openTap(name string) (*os.File, error) {
	r, err := newIfreq(name)
	if err != nil {
		return nil, err
	}
	tun, err := os.OpenFile("/dev/net/tun", os.O_RDWR, 0)
	if err != nil {
		return nil, err
	}
	*(*uint16)(unsafe.Pointer(&r.data[0])) = iffTap | iffNoPI | iffVnetHdr
	if err := ioctl(tun.Fd(), tunSetIff, unsafe.Pointer(r)); err != nil {
		tun.Close()
		return nil, fmt.Errorf("opening tap %s: %w", name, err)
	}
	return tun, nil
}
