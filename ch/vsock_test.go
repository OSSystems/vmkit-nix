package ch

import (
	"bufio"
	"context"
	"net"
	"path/filepath"
	"testing"
	"time"
)

// fakeVsock answers the CONNECT handshake like cloud-hypervisor: OK for
// port 5002, an error line otherwise, then sends one line.
func fakeVsock(t *testing.T) string {
	t.Helper()
	socket := filepath.Join(t.TempDir(), "vsock.sock")
	ln, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ln.Close() })
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			r := bufio.NewReader(conn)
			line, _ := r.ReadString('\n')
			if line != "CONNECT 5002\n" {
				conn.Write([]byte("refused\n"))
				conn.Close()
				continue
			}
			// The reply and the guest's first bytes arrive together.
			conn.Write([]byte("OK 1073741824\nhello\n"))
			conn.Close()
		}
	}()
	return socket
}

func TestDialGuest(t *testing.T) {
	socket := fakeVsock(t)
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	conn, err := DialGuest(ctx, socket, 5002)
	if err != nil {
		t.Fatal(err)
	}
	line, err := bufio.NewReader(conn).ReadString('\n')
	if err != nil || line != "hello\n" {
		t.Fatalf("guest data after the handshake: %q, %v", line, err)
	}
	if _, err := DialGuest(ctx, socket, 9); err == nil {
		t.Fatal("a refused port was reached")
	}
}
