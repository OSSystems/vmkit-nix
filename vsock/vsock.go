// Package vsock is the guest side of virtio-vsock: connecting to the host
// and listening for it, without cgo.
package vsock

import (
	"fmt"
	"os"
	"syscall"
	"unsafe"
)

const (
	afVsock = 40
	// CIDHost is the host's address as seen from a guest.
	CIDHost = 2
	cidAny  = 0xFFFFFFFF
)

type sockaddrVM struct {
	family    uint16
	reserved1 uint16
	port      uint32
	cid       uint32
	flags     uint8
	zero      [3]uint8
}

func socketFD() (int, error) {
	return syscall.Socket(afVsock, syscall.SOCK_STREAM|syscall.SOCK_CLOEXEC, 0)
}

func call(trap uintptr, fd int, cid, port uint32) error {
	sa := sockaddrVM{family: afVsock, port: port, cid: cid}
	if _, _, errno := syscall.Syscall(trap, uintptr(fd), uintptr(unsafe.Pointer(&sa)), unsafe.Sizeof(sa)); errno != 0 {
		return errno
	}
	return nil
}

// pollable turns a socket into a file whose reads and writes honour
// deadlines.
func pollable(fd int, name string) (*os.File, error) {
	if err := syscall.SetNonblock(fd, true); err != nil {
		syscall.Close(fd)
		return nil, err
	}
	return os.NewFile(uintptr(fd), name), nil
}

// Dial connects from a guest to port on cid, usually CIDHost.
func Dial(cid, port uint32) (*os.File, error) {
	fd, err := socketFD()
	if err != nil {
		return nil, err
	}
	if err := call(syscall.SYS_CONNECT, fd, cid, port); err != nil {
		syscall.Close(fd)
		return nil, fmt.Errorf("vsock connect %d:%d: %w", cid, port, err)
	}
	return pollable(fd, "vsock")
}

// Listener accepts vsock connections on one port.
type Listener struct{ fd int }

// Listen listens on port for connections from the host.
func Listen(port uint32) (*Listener, error) {
	fd, err := socketFD()
	if err != nil {
		return nil, err
	}
	if err := call(syscall.SYS_BIND, fd, cidAny, port); err != nil {
		syscall.Close(fd)
		return nil, fmt.Errorf("vsock bind %d: %w", port, err)
	}
	if err := syscall.Listen(fd, 16); err != nil {
		syscall.Close(fd)
		return nil, err
	}
	return &Listener{fd: fd}, nil
}

// Accept blocks for the next connection.
func (l *Listener) Accept() (*os.File, error) {
	for {
		nfd, _, errno := syscall.Syscall6(syscall.SYS_ACCEPT4, uintptr(l.fd), 0, 0, syscall.SOCK_CLOEXEC, 0, 0)
		if errno == syscall.EINTR {
			continue
		}
		if errno != 0 {
			return nil, errno
		}
		return pollable(int(nfd), "vsock")
	}
}

func (l *Listener) Close() error { return syscall.Close(l.fd) }
