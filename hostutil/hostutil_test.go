package hostutil

import (
	"os"
	"path/filepath"
	"testing"
)

func TestAtomicWriteAndReadBounded(t *testing.T) {
	path := filepath.Join(t.TempDir(), "f")
	if err := AtomicWrite(path, []byte("one"), 0640); err != nil {
		t.Fatal(err)
	}
	if err := AtomicWrite(path, []byte("two"), 0640); err != nil {
		t.Fatal(err)
	}
	fi, err := os.Stat(path)
	if err != nil || fi.Mode().Perm() != 0640 {
		t.Fatalf("mode %v, %v", fi.Mode(), err)
	}
	if b, err := ReadBounded(path, 3); err != nil || string(b) != "two" {
		t.Fatalf("read %q, %v", b, err)
	}
	if _, err := ReadBounded(path, 2); err == nil {
		t.Fatal("read past the limit")
	}
	entries, _ := os.ReadDir(filepath.Dir(path))
	if len(entries) != 1 {
		t.Fatalf("leftover temporary files: %v", entries)
	}
}
