// Package control is the guest-to-host control channel: one JSON request
// line a guest sends to a vsock port, one JSON reply line back. vmkit
// defines the framing and the "stats" request its memory policy needs;
// callers add their own operations and reply fields.
package control

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"strconv"
	"strings"
	"time"

	"github.com/OSSystems/vmkit-nix/memory"
)

// Request is one newline-delimited JSON request. The socket it came in on
// identifies the VM: the host side of a guest's vsock port P is the VM's
// own <vsock.sock>_P, so nothing in the payload is trusted to name it.
type Request struct {
	Op              string   `json:"op"`
	MemAvailableKiB *int64   `json:"memAvailableKiB,omitempty"`
	MemTotalKiB     *int64   `json:"memTotalKiB,omitempty"`
	PSISomeAvg10    *float64 `json:"psiSomeAvg10,omitempty"`
	PSIFullAvg10    *float64 `json:"psiFullAvg10,omitempty"`
}

// Reply is the part of a reply every operation has; embed it to add fields.
type Reply struct {
	OK    bool   `json:"ok,omitempty"`
	Error string `json:"error,omitempty"`
}

// ReadRequest reads one request line; a failure comes back as the reply to
// send.
func ReadRequest(r *bufio.Reader) (Request, *Reply) {
	var req Request
	line, err := r.ReadBytes('\n')
	if err != nil && (!errors.Is(err, io.EOF) || len(line) == 0) {
		return req, &Reply{Error: "expected one JSON line"}
	}
	if json.Unmarshal(line, &req) != nil {
		return req, &Reply{Error: "invalid JSON"}
	}
	return req, nil
}

// Stats is the report a "stats" request carries, received at now.
func (req Request) Stats(now time.Time) (memory.GuestStats, *Reply) {
	if req.MemAvailableKiB == nil || req.MemTotalKiB == nil || req.PSISomeAvg10 == nil || req.PSIFullAvg10 == nil {
		return memory.GuestStats{}, &Reply{Error: "incomplete stats"}
	}
	s := memory.GuestStats{MemAvailableKiB: *req.MemAvailableKiB, MemTotalKiB: *req.MemTotalKiB, PSISomeAvg10: *req.PSISomeAvg10, PSIFullAvg10: *req.PSIFullAvg10, ReceivedAt: now}
	if !s.Valid() {
		return s, &Reply{Error: "invalid stats"}
	}
	return s, nil
}

// StatsRequest is the "stats" request a guest sends, read from the text of
// its /proc/meminfo and /proc/pressure/memory. A guest without PSI reports
// no pressure.
func StatsRequest(meminfo, pressure string) (Request, error) {
	req := Request{Op: "stats"}
	for _, line := range strings.Split(meminfo, "\n") {
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}
		n, err := strconv.ParseInt(fields[1], 10, 64)
		if err != nil {
			continue
		}
		switch fields[0] {
		case "MemAvailable:":
			req.MemAvailableKiB = &n
		case "MemTotal:":
			req.MemTotalKiB = &n
		}
	}
	if req.MemAvailableKiB == nil || req.MemTotalKiB == nil {
		return req, errors.New("no MemAvailable or MemTotal")
	}
	some, full := 0.0, 0.0
	for _, line := range strings.Split(pressure, "\n") {
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}
		v, ok := strings.CutPrefix(fields[1], "avg10=")
		if !ok {
			continue
		}
		f, err := strconv.ParseFloat(v, 64)
		if err != nil {
			continue
		}
		switch fields[0] {
		case "some":
			some = f
		case "full":
			full = f
		}
	}
	req.PSISomeAvg10, req.PSIFullAvg10 = &some, &full
	return req, nil
}

// Serve hands every connection on ln to handle until ln closes.
func Serve(ln net.Listener, handle func(net.Conn)) {
	for {
		conn, err := ln.Accept()
		if err != nil {
			var ne net.Error
			if errors.As(err, &ne) && ne.Timeout() {
				continue
			}
			return
		}
		go handle(conn)
	}
}

// Answer reads one request from conn within 5 s and writes the reply answer
// makes, then tells delivered, when not nil, whether the reply was written.
func Answer(conn net.Conn, answer func(*bufio.Reader) (reply any, delivered func(bool))) {
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(5 * time.Second))
	reply, delivered := answer(bufio.NewReader(io.LimitReader(conn, 4096)))
	raw, _ := json.Marshal(reply)
	_, err := conn.Write(append(raw, '\n'))
	if delivered != nil {
		delivered(err == nil)
	}
}

// ReadLine reads one newline-terminated line of at most limit bytes.
func ReadLine(r *bufio.Reader, limit int) ([]byte, error) {
	var line []byte
	for {
		chunk, isPrefix, err := r.ReadLine()
		if err != nil {
			return nil, err
		}
		line = append(line, chunk...)
		if len(line) > limit {
			return nil, fmt.Errorf("line longer than %d bytes", limit)
		}
		if !isPrefix {
			return line, nil
		}
	}
}
