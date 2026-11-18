package memory

import (
	"testing"
	"time"
)

func TestMemoryDecision(t *testing.T) {
	now := time.Now()
	stats := func(availGiB int64, psi float64) GuestStats {
		return GuestStats{MemAvailableKiB: availGiB << 20, MemTotalKiB: 8 << 20, PSISomeAvg10: psi, ReceivedAt: now.Add(-time.Second)}
	}
	base := VMMemory{BootBytes: 2 * GiB, MaxBytes: 10 * GiB, ActualBytes: 4 * GiB}
	roomy := HostMemory{Available: 40 * GiB, Reserve: 6 * GiB}
	tests := []struct {
		name   string
		vm     func(*VMMemory)
		host   HostMemory
		action MemoryAction
		target int64
	}{
		{"psi plugs a step", func(v *VMMemory) { v.Stats = stats(2, 6) }, roomy, MemoryPlug, 5 * GiB},
		{"low available plugs", func(v *VMMemory) { v.Stats = stats(0, 0) }, roomy, MemoryPlug, 5 * GiB},
		{"plug capped at hotplug size", func(v *VMMemory) { v.Stats = stats(0, 50); v.ActualBytes = 10 * GiB }, roomy, MemoryHold, 10 * GiB},
		{"host reserve denies", func(v *VMMemory) { v.Stats = stats(0, 50) }, HostMemory{Available: 7 * GiB, Reserve: 6 * GiB, Pending: 1}, MemoryDenied, 4 * GiB},
		{"pending memory denies", func(v *VMMemory) { v.Stats = stats(0, 50) }, HostMemory{Available: 10 * GiB, Reserve: 6 * GiB, Pending: 4 * GiB}, MemoryDenied, 4 * GiB},
		{"plug that exactly fits", func(v *VMMemory) { v.Stats = stats(0, 50) }, HostMemory{Available: 7 * GiB, Reserve: 6 * GiB}, MemoryPlug, 5 * GiB},
		{"stale stats freeze", func(v *VMMemory) { v.Stats = stats(0, 50); v.Stats.ReceivedAt = now.Add(-11 * time.Second) }, roomy, MemoryHold, 4 * GiB},
		{"no stats yet", func(v *VMMemory) {}, roomy, MemoryHold, 4 * GiB},
		{"waits for stats after resize", func(v *VMMemory) { v.Stats = stats(0, 50); v.ResizedAt = now }, roomy, MemoryHold, 4 * GiB},
		{"stats after resize count", func(v *VMMemory) { v.Stats = stats(0, 50); v.ResizedAt = now.Add(-2 * time.Second) }, roomy, MemoryPlug, 5 * GiB},
		{"calm long enough unplugs", func(v *VMMemory) { v.Stats = stats(4, 0); v.CalmSince = now.Add(-31 * time.Second) }, roomy, MemoryUnplug, 3 * GiB},
		{"calm too short holds", func(v *VMMemory) { v.Stats = stats(4, 0); v.CalmSince = now.Add(-20 * time.Second) }, roomy, MemoryHold, 4 * GiB},
		{"spare but some pressure holds", func(v *VMMemory) { v.Stats = stats(4, 1); v.CalmSince = now.Add(-time.Minute) }, roomy, MemoryHold, 4 * GiB},
		{"never below boot", func(v *VMMemory) { v.Stats = stats(4, 0); v.ActualBytes = 2 * GiB; v.CalmSince = now.Add(-time.Minute) }, roomy, MemoryHold, 2 * GiB},
		{"partial unplug floors at boot", func(v *VMMemory) {
			v.Stats = stats(4, 0)
			v.ActualBytes = 2*GiB + 512*MiB
			v.CalmSince = now.Add(-time.Minute)
		}, roomy, MemoryUnplug, 2 * GiB},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vm := base
			tt.vm(&vm)
			action, target := Decide(vm, tt.host, now)
			if action != tt.action || target != tt.target {
				t.Fatalf("decision = %s %d MiB, want %s %d MiB", action, target/MiB, tt.action, tt.target/MiB)
			}
		})
	}
}

func TestPlugPendingUntilGuestShowsIt(t *testing.T) {
	t0 := time.Now()
	pressured := func(at time.Time, totalMiB int64) GuestStats {
		return GuestStats{MemAvailableKiB: 256 << 10, MemTotalKiB: totalMiB << 10, PSISomeAvg10: 30, ReceivedAt: at}
	}
	m := VMMemory{BootBytes: 2 * GiB, MaxBytes: 10 * GiB, ActualBytes: 4 * GiB}
	m.Observe(pressured(t0, 3900))
	m.Grant(4*GiB, 5*GiB, t0.Add(time.Second))
	roomy := HostMemory{Available: 40 * GiB, Reserve: 6 * GiB}
	// Reported after the resize, but virtio-mem has not onlined the block yet.
	m.Observe(pressured(t0.Add(3*time.Second), 3900))
	if m.PendingPlug != GiB {
		t.Fatalf("Pending = %d MiB, want the plug still Pending", m.PendingPlug/MiB)
	}
	if action, _ := Decide(m, roomy, t0.Add(4*time.Second)); action != MemoryHold {
		t.Fatalf("plugged again before the last plug showed: %s", action)
	}
	m.Observe(pressured(t0.Add(5*time.Second), 3900+1024-100))
	if m.PendingPlug != 0 {
		t.Fatal("plug still Pending after MemTotal grew by it")
	}
	if action, target := Decide(m, roomy, t0.Add(6*time.Second)); action != MemoryPlug || target != 5*GiB {
		t.Fatalf("decision = %s %d MiB", action, target/MiB)
	}
	// A plug that never shows stops counting after PlugSettle.
	m.Grant(5*GiB, 6*GiB, t0.Add(7*time.Second))
	m.Observe(pressured(t0.Add(8*time.Second+PlugSettle), 3900))
	if m.PendingPlug != 0 {
		t.Fatal("plug Pending forever")
	}
}

// After an unplug the guest must stay calm for another unplugCalm: one step
// per period, not one per report.
func TestUnplugOncePerCalmPeriod(t *testing.T) {
	t0 := time.Now()
	calm := func(at time.Time) GuestStats {
		return GuestStats{MemAvailableKiB: 6 << 20, MemTotalKiB: 8 << 20, ReceivedAt: at}
	}
	roomy := HostMemory{Available: 40 * GiB, Reserve: 6 * GiB}
	m := VMMemory{BootBytes: 2 * GiB, MaxBytes: 10 * GiB, ActualBytes: 8 * GiB}
	m.Observe(calm(t0))
	m.Observe(calm(t0.Add(31 * time.Second)))
	action, target := Decide(m, roomy, t0.Add(31*time.Second))
	if action != MemoryUnplug {
		t.Fatalf("decision = %s", action)
	}
	m.Grant(m.ActualBytes, target, t0.Add(31*time.Second))
	m.ActualBytes = target
	m.Observe(calm(t0.Add(33 * time.Second)))
	if action, _ := Decide(m, roomy, t0.Add(33*time.Second)); action != MemoryHold {
		t.Fatalf("unplugged again 2 s after an unplug: %s", action)
	}
	m.Observe(calm(t0.Add(64 * time.Second)))
	if action, _ := Decide(m, roomy, t0.Add(64*time.Second)); action != MemoryUnplug {
		t.Fatalf("no unplug after another calm period: %s", action)
	}
}
