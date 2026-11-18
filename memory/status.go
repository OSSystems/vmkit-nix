package memory

import (
	"encoding/json"
	"os"
	"path/filepath"
	"time"

	"github.com/OSSystems/vmkit-nix/hostutil"
)

// Status is what one pool of VMs publishes about the memory it holds, so
// the other pools on the host can account for it. A pool is whatever one
// controller manages.
type Status struct {
	UpdatedAt      time.Time `json:"updatedAt"`
	CommittedBytes int64     `json:"committedBytes"`
	PendingBytes   int64     `json:"pendingBytes"`
	// DeniedAt is when one of the pool's VMs was last denied a plug.
	DeniedAt time.Time `json:"deniedAt,omitzero"`
}

// StatusStale is how old a published status may be before it counts as
// gone: its controller stopped.
const StatusStale = 30 * time.Second

// WriteStatus publishes s at path; an empty path publishes nothing.
func WriteStatus(path string, s Status) error {
	if path == "" {
		return nil
	}
	raw, err := json.Marshal(s)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}
	return hostutil.AtomicWrite(path, raw, 0644)
}

// ReadStatus reads a published status; a missing or stale one is empty.
func ReadStatus(path string, now time.Time) (Status, bool) {
	var s Status
	raw, err := hostutil.ReadBounded(path, 4096)
	if err != nil || json.Unmarshal(raw, &s) != nil || now.Sub(s.UpdatedAt) > StatusStale {
		return Status{}, false
	}
	return s, true
}

// ReadStatuses reads several pools' statuses as one.
func ReadStatuses(paths []string, now time.Time) Status {
	var sum Status
	for _, path := range paths {
		s, ok := ReadStatus(path, now)
		if !ok {
			continue
		}
		sum.CommittedBytes += s.CommittedBytes
		sum.PendingBytes += s.PendingBytes
		if s.DeniedAt.After(sum.DeniedAt) {
			sum.DeniedAt = s.DeniedAt
		}
		if sum.UpdatedAt.IsZero() || s.UpdatedAt.Before(sum.UpdatedAt) {
			sum.UpdatedAt = s.UpdatedAt
		}
	}
	return sum
}

// Pools sharing a host come in two ranks. A first pool, say long-lived VMs
// people work in, grows before a yielding pool, say throwaway CI VMs,
// except into the memory the yielding pool is guaranteed. Each pool
// publishes its Status and reads the other rank's.

// Ahead is what a yielding pool sees of the first pools on its host.
type Ahead struct {
	Status Status
	// Guaranteed is what the yielding pool may hold whatever the first
	// pools want.
	Guaranteed int64
}

// Yields reports whether a yielding pool holding committed gives way: past
// its guarantee it counts what the first pools hold as taken.
func (a Ahead) Yields(committed int64) bool { return committed > a.Guaranteed }

// Starved reports whether a first pool was denied memory recently; a
// yielding pool past its guarantee then neither grows nor boots.
func (a Ahead) Starved(now time.Time) bool {
	return !a.Status.DeniedAt.IsZero() && now.Sub(a.Status.DeniedAt) < HostFullHold
}

// Owed is what a first pool keeps free for a yielding pool: the part of its
// guarantee it does not hold yet.
func Owed(guaranteed int64, behind Status) int64 {
	return max(0, guaranteed-behind.CommittedBytes)
}
