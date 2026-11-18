package memory

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestStatusRoundTripAndStaleness(t *testing.T) {
	dir := t.TempDir()
	now := time.Date(2026, 11, 18, 12, 0, 0, 0, time.UTC)
	path := filepath.Join(dir, "pools", "runners.json")
	s := Status{UpdatedAt: now, CommittedBytes: 3 * GiB, PendingBytes: GiB, DeniedAt: now.Add(-time.Second)}
	if err := WriteStatus(path, s); err != nil {
		t.Fatal(err)
	}
	got, ok := ReadStatus(path, now.Add(time.Second))
	if !ok || !got.UpdatedAt.Equal(s.UpdatedAt) || got.CommittedBytes != s.CommittedBytes || !got.DeniedAt.Equal(s.DeniedAt) {
		t.Fatalf("read %+v, %v", got, ok)
	}
	if _, ok := ReadStatus(path, now.Add(StatusStale+time.Second)); ok {
		t.Fatal("a stale status counted")
	}
	if _, ok := ReadStatus(filepath.Join(dir, "missing.json"), now); ok {
		t.Fatal("a missing status counted")
	}
	if err := WriteStatus("", s); err != nil {
		t.Fatal(err)
	}
}

func TestReadStatusesSums(t *testing.T) {
	dir := t.TempDir()
	now := time.Date(2026, 11, 18, 12, 0, 0, 0, time.UTC)
	a, b, stale := filepath.Join(dir, "a.json"), filepath.Join(dir, "b.json"), filepath.Join(dir, "c.json")
	WriteStatus(a, Status{UpdatedAt: now, CommittedBytes: GiB, PendingBytes: GiB, DeniedAt: now.Add(-5 * time.Second)})
	WriteStatus(b, Status{UpdatedAt: now.Add(-time.Second), CommittedBytes: 2 * GiB, DeniedAt: now.Add(-time.Second)})
	WriteStatus(stale, Status{UpdatedAt: now.Add(-time.Hour), CommittedBytes: 100 * GiB})
	os.WriteFile(filepath.Join(dir, "garbage.json"), []byte("{"), 0644)
	sum := ReadStatuses([]string{a, b, stale, filepath.Join(dir, "garbage.json")}, now)
	if sum.CommittedBytes != 3*GiB || sum.PendingBytes != GiB || !sum.DeniedAt.Equal(now.Add(-time.Second)) {
		t.Fatalf("sum = %+v", sum)
	}
}

func TestRanks(t *testing.T) {
	now := time.Date(2026, 11, 18, 12, 0, 0, 0, time.UTC)
	ahead := Ahead{Guaranteed: 8 * GiB, Status: Status{DeniedAt: now.Add(-10 * time.Second)}}
	if ahead.Yields(8*GiB) || !ahead.Yields(8*GiB+1) {
		t.Fatal("yields only past the guarantee")
	}
	if !ahead.Starved(now) || ahead.Starved(now.Add(HostFullHold)) {
		t.Fatal("starved only within HostFullHold of a denial")
	}
	if (Ahead{}).Starved(now) {
		t.Fatal("starved without a denial")
	}
	for _, c := range []struct{ held, owed int64 }{{0, 8 * GiB}, {5 * GiB, 3 * GiB}, {30 * GiB, 0}} {
		if got := Owed(8*GiB, Status{CommittedBytes: c.held}); got != c.owed {
			t.Errorf("holding %d: owed %d, want %d", c.held, got, c.owed)
		}
	}
}
