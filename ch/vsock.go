package ch

import (
	"context"
	"fmt"
	"net"
	"strings"
	"time"
)

// DialGuest connects to a guest's vsock port through cloud-hypervisor's
// host-side socket, which takes a "CONNECT <port>" line first.
func DialGuest(ctx context.Context, vsockSocket string, port int) (net.Conn, error) {
	conn, err := (&net.Dialer{}).DialContext(ctx, "unix", vsockSocket)
	if err != nil {
		return nil, err
	}
	if deadline, ok := ctx.Deadline(); ok {
		conn.SetDeadline(deadline)
	}
	if _, err := fmt.Fprintf(conn, "CONNECT %d\n", port); err != nil {
		conn.Close()
		return nil, err
	}
	// Byte by byte, so nothing the guest sends after the reply is buffered
	// away.
	var line []byte
	one := make([]byte, 1)
	for len(line) < 64 {
		if _, err := conn.Read(one); err != nil {
			conn.Close()
			return nil, fmt.Errorf("vsock port %d: %w", port, err)
		}
		if one[0] == '\n' {
			break
		}
		line = append(line, one[0])
	}
	if !strings.HasPrefix(string(line), "OK ") {
		conn.Close()
		return nil, fmt.Errorf("vsock port %d: %q", port, line)
	}
	conn.SetDeadline(time.Time{})
	return conn, nil
}

// GuestPortSocket is where cloud-hypervisor forwards a guest's connections
// to host port: the vsock socket's path with "_<port>" appended. Whoever
// listens there is reached by that guest only.
func GuestPortSocket(vsockSocket string, port int) string {
	return fmt.Sprintf("%s_%d", vsockSocket, port)
}
