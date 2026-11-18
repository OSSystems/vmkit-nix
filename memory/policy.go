// Package memory is vmkit's elastic memory policy: guests boot small and
// grow or shrink 1 GiB at a time through virtio-mem, by their memory
// pressure, as long as the host keeps its reserve. Pools of VMs sharing a
// host publish a Status so each accounts for the others.
package memory

import (
	"math"
	"time"
)

const (
	MiB = int64(1) << 20
	GiB = int64(1) << 30
	// MemoryStep is how much memory one plug or unplug moves.
	MemoryStep = GiB
	// StatsStale is how old guest stats may be before memory stops changing.
	StatsStale = 10 * time.Second
	// UnplugCalm is how long a guest must have had spare memory and no
	// pressure before memory is taken back.
	UnplugCalm = 30 * time.Second
	// PlugSettle is how long a plug may take to show in the guest's MemTotal
	// before it stops counting as pending.
	PlugSettle = time.Minute
	// PlugSlack absorbs memory the guest kernel keeps for itself out of a plug.
	PlugSlack = 128 * MiB
	// HostFullHold keeps admission closed for this long after a plug was
	// denied: running guests come first.
	HostFullHold = 30 * time.Second
)

// GuestStats is one report a guest sent on the control channel.
type GuestStats struct {
	MemAvailableKiB int64     `json:"memAvailableKiB"`
	MemTotalKiB     int64     `json:"memTotalKiB"`
	PSISomeAvg10    float64   `json:"psiSomeAvg10"`
	PSIFullAvg10    float64   `json:"psiFullAvg10"`
	ReceivedAt      time.Time `json:"receivedAt"`
}

func (s GuestStats) Valid() bool {
	return s.MemAvailableKiB >= 0 && s.MemTotalKiB > 0 && s.MemAvailableKiB <= s.MemTotalKiB &&
		finiteNonNegative(s.PSISomeAvg10) && finiteNonNegative(s.PSIFullAvg10) && s.PSISomeAvg10 <= 100 && s.PSIFullAvg10 <= 100
}

func finiteNonNegative(v float64) bool { return !math.IsNaN(v) && !math.IsInf(v, 0) && v >= 0 }

func (s GuestStats) Available() int64 { return s.MemAvailableKiB << 10 }

// Pressured is when a guest needs more memory.
func (s GuestStats) Pressured() bool { return s.PSISomeAvg10 > 5 || s.Available() < GiB }

// Calm is when a guest could give memory back.
func (s GuestStats) Calm() bool { return s.PSISomeAvg10 < 0.1 && s.Available() > 3*GiB }

// HostMemory is the host side of a memory decision, in bytes.
type HostMemory struct {
	Available int64
	Reserve   int64
	// Pending is memory promised but not yet taken: VMs that have not
	// reported yet and plugs their guest has not reported since.
	Pending int64
}

// Spare is what may still be handed out without touching the reserve.
func (h HostMemory) Spare() int64 { return h.Available - h.Reserve - h.Pending }

type MemoryAction int

const (
	MemoryHold MemoryAction = iota
	MemoryPlug
	MemoryUnplug
	MemoryDenied
)

func (a MemoryAction) String() string {
	return [...]string{"hold", "plug", "unplug", "denied"}[a]
}

// VMMemory is what the policy knows about one VM.
type VMMemory struct {
	BootBytes int64
	MaxBytes  int64
	// ActualBytes is boot memory plus what virtio-mem has plugged; an unplug
	// may leave it above the requested size.
	ActualBytes int64
	Stats       GuestStats
	// ResizedAt is the last resize request; stats older than it do not show
	// its effect yet.
	ResizedAt time.Time
	// CalmSince is when the guest last became calm; zero when it is not.
	CalmSince time.Time
	// PendingPlug is the last plug while the guest's MemTotal does not show
	// it yet; PlugBaseKiB is the MemTotal it was granted on.
	PendingPlug int64
	PlugBaseKiB int64
}

// NewVMMemory is a new VM's memory: the boot size, growing up to maxBytes.
func NewVMMemory(bootBytes, maxBytes int64) VMMemory {
	return VMMemory{BootBytes: bootBytes, MaxBytes: maxBytes, ActualBytes: bootBytes}
}

// Observe takes a guest report into account: calm tracking, and the end of
// a pending plug once MemTotal shows it.
func (m *VMMemory) Observe(s GuestStats) {
	if m.Stats.ReceivedAt.After(s.ReceivedAt) {
		return
	}
	m.Stats = s
	if s.Calm() {
		if m.CalmSince.IsZero() {
			m.CalmSince = s.ReceivedAt
		}
	} else {
		m.CalmSince = time.Time{}
	}
	if m.PendingPlug > 0 && s.ReceivedAt.After(m.ResizedAt) &&
		(s.MemTotalKiB<<10 >= m.PlugBaseKiB<<10+m.PendingPlug-PlugSlack || s.ReceivedAt.Sub(m.ResizedAt) > PlugSettle) {
		m.PendingPlug = 0
	}
}

// Grant records a resize from actual to target before it is requested, so
// a plug counts as pending from the moment it is decided. It returns the
// state to restore if the request fails.
func (m *VMMemory) Grant(actual, target int64, at time.Time) VMMemory {
	previous := *m
	m.ResizedAt = at
	m.PendingPlug = 0
	if target > actual {
		m.PendingPlug = target - actual
		m.PlugBaseKiB = m.Stats.MemTotalKiB
	} else {
		// One unplug per calm period: the guest must stay calm for another
		// UnplugCalm before the next step.
		m.CalmSince = time.Time{}
	}
	return previous
}

// Pending is memory this VM was promised but has not been seen to take: its
// boot memory until it first reports, then a plug its guest does not show
// yet.
func (m VMMemory) Pending() int64 {
	if m.Stats.ReceivedAt.IsZero() {
		return m.BootBytes
	}
	return m.PendingPlug
}

// Committed is what this VM was given, boot plus plugged plus a plug in
// flight, and room for its next plug: memory it may touch at any moment.
func (m VMMemory) Committed() int64 {
	return m.ActualBytes + m.PendingPlug + MemoryStep
}

// Decide decides one VM's next memory change: plug 1 GiB under guest
// pressure if the host keeps its reserve, unplug 1 GiB after 30 s of calm,
// never below boot memory, and nothing on stale stats, before the guest
// reported after the last resize, or while the last plug is not visible yet.
func Decide(vm VMMemory, host HostMemory, now time.Time) (MemoryAction, int64) {
	s := vm.Stats
	if s.ReceivedAt.IsZero() || now.Sub(s.ReceivedAt) > StatsStale || s.ReceivedAt.After(now.Add(time.Second)) || !s.Valid() {
		return MemoryHold, vm.ActualBytes
	}
	if !vm.ResizedAt.IsZero() && !s.ReceivedAt.After(vm.ResizedAt) || vm.PendingPlug > 0 {
		return MemoryHold, vm.ActualBytes
	}
	if s.Pressured() {
		target := min(vm.MaxBytes, vm.ActualBytes+MemoryStep)
		if target <= vm.ActualBytes {
			return MemoryHold, vm.ActualBytes
		}
		if host.Spare() < target-vm.ActualBytes {
			return MemoryDenied, vm.ActualBytes
		}
		return MemoryPlug, target
	}
	if s.Calm() && !vm.CalmSince.IsZero() && now.Sub(vm.CalmSince) >= UnplugCalm {
		target := max(vm.BootBytes, vm.ActualBytes-MemoryStep)
		if target < vm.ActualBytes {
			return MemoryUnplug, target
		}
	}
	return MemoryHold, vm.ActualBytes
}

// Reserve is what the host keeps for itself: the larger of a share of its
// memory and a floor.
func Reserve(totalBytes int64, percent, minGiB float64) int64 {
	return max(int64(float64(totalBytes)*percent/100), int64(minGiB*float64(GiB)))
}
