package memory

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// docs/contracts.md: the pool status files every controller writes and reads.

func contractFile(t *testing.T, name string) string {
	t.Helper()
	return filepath.Join("..", "testdata", "contract", name)
}

func TestStatusContract(t *testing.T) {
	updated := time.Date(2026, 11, 18, 12, 0, 0, 5e8, time.UTC)
	want := Status{UpdatedAt: updated, CommittedBytes: 3 * GiB, PendingBytes: GiB, DeniedAt: time.Date(2026, 11, 18, 11, 59, 59, 0, time.UTC)}
	for _, name := range []string{"status.json", "status-offset.json"} {
		got, ok := ReadStatus(contractFile(t, name), updated.Add(time.Second))
		if !ok || !got.UpdatedAt.Equal(want.UpdatedAt) || got.CommittedBytes != want.CommittedBytes || got.PendingBytes != want.PendingBytes || !got.DeniedAt.Equal(want.DeniedAt) {
			t.Errorf("%s: %+v, %v", name, got, ok)
		}
	}
	minimal, ok := ReadStatus(contractFile(t, "status-minimal.json"), updated)
	if !ok || !minimal.DeniedAt.IsZero() || minimal.CommittedBytes != 0 {
		t.Errorf("status-minimal.json: %+v, %v", minimal, ok)
	}

	path := filepath.Join(t.TempDir(), "pool.json")
	if err := WriteStatus(path, want); err != nil {
		t.Fatal(err)
	}
	written, _ := os.ReadFile(path)
	fixture, _ := os.ReadFile(contractFile(t, "status.json"))
	if string(written) != strings.TrimSpace(string(fixture)) {
		t.Errorf("written %s, want %s", written, fixture)
	}
	if err := WriteStatus(path, Status{UpdatedAt: updated.Truncate(time.Second)}); err != nil {
		t.Fatal(err)
	}
	written, _ = os.ReadFile(path)
	if fixture, _ := os.ReadFile(contractFile(t, "status-minimal.json")); string(written) != strings.TrimSpace(string(fixture)) {
		t.Errorf("written %s, want %s", written, fixture)
	}
	if fi, err := os.Stat(path); err != nil || fi.Mode().Perm() != 0644 {
		t.Errorf("mode %v, %v", fi.Mode(), err)
	}
}

func TestStatusContractRejects(t *testing.T) {
	dir := t.TempDir()
	now := time.Date(2026, 11, 18, 12, 0, 0, 0, time.UTC)
	big, _ := json.Marshal(map[string]any{"updatedAt": now, "committedBytes": 1, "pendingBytes": 0, "pad": strings.Repeat("x", 4096)})
	for name, content := range map[string][]byte{
		"stale":   []byte(`{"updatedAt":"2026-11-18T11:59:29Z","committedBytes":1,"pendingBytes":0}`),
		"garbage": []byte(`{"updatedAt":`),
		"big":     big,
	} {
		path := filepath.Join(dir, name)
		os.WriteFile(path, content, 0644)
		if s, ok := ReadStatus(path, now); ok {
			t.Errorf("%s counted: %+v", name, s)
		}
	}
}
