// Package ch drives cloud-hypervisor: its command line, its REST API (CH 52
// OpenAPI) over the API socket and the host side of its vsock device.
package ch

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"time"
)

// Client talks to one cloud-hypervisor's REST API over its Unix socket.
type Client struct {
	http *http.Client
}

// NewClient talks to the API socket at socket.
func NewClient(socket string) *Client {
	return &Client{http: &http.Client{
		Timeout: 5 * time.Second,
		// One connection per request: idle keep-alive connections would pile
		// up against the VMM's small connection limit.
		Transport: &http.Transport{
			DisableKeepAlives: true,
			DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
				return (&net.Dialer{}).DialContext(ctx, "unix", socket)
			},
		},
	}}
}

func (c *Client) do(ctx context.Context, method, endpoint string, body, out any) error {
	var reader io.Reader
	if body != nil {
		raw, err := json.Marshal(body)
		if err != nil {
			return err
		}
		reader = bytes.NewReader(raw)
	}
	req, err := http.NewRequestWithContext(ctx, method, "http://localhost/api/v1/"+endpoint, reader)
	if err != nil {
		return err
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 4<<20))
	if err != nil {
		return err
	}
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("cloud-hypervisor %s: %s: %s", endpoint, resp.Status, bytes.TrimSpace(raw))
	}
	if out != nil {
		return json.Unmarshal(raw, out)
	}
	return nil
}

// VMInfo is the part of vm.info vmkit reads.
type VMInfo struct {
	State string `json:"state"`
	// MemoryActualSize is boot memory plus what virtio-mem has plugged.
	MemoryActualSize int64 `json:"memory_actual_size"`
}

func (c *Client) Info(ctx context.Context) (VMInfo, error) {
	var info VMInfo
	err := c.do(ctx, http.MethodGet, "vm.info", nil, &info)
	return info, err
}

// ResizeMemory asks for desiredBytes of guest RAM; virtio-mem plugs or
// unplugs the difference above the boot size.
func (c *Client) ResizeMemory(ctx context.Context, desiredBytes int64) error {
	return c.do(ctx, http.MethodPut, "vm.resize", struct {
		DesiredRAM int64 `json:"desired_ram"`
	}{desiredBytes}, nil)
}

// PowerButton presses the guest's ACPI power button.
func (c *Client) PowerButton(ctx context.Context) error {
	return c.do(ctx, http.MethodPut, "vm.power-button", nil, nil)
}
