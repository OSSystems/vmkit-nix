package virtiofs

import (
	"os"
	"path/filepath"
	"slices"
	"testing"
)

func TestStorePaths(t *testing.T) {
	dir := t.TempDir()
	write := func(content string) string {
		path := filepath.Join(dir, "store-paths")
		os.WriteFile(path, []byte(content), 0644)
		return path
	}
	paths, err := StorePaths(write("/nix/store/aaa-t3code\n/nix/store/bbb-glibc\n\n"))
	if err != nil || !slices.Equal(paths, []string{"/nix/store/aaa-t3code", "/nix/store/bbb-glibc"}) {
		t.Fatalf("paths = %v, %v", paths, err)
	}
	for _, bad := range []string{"/etc/shadow\n", "/nix/store/aaa/../../etc\n", "/nix/store/..\n", "/nix/store/\n"} {
		if _, err := StorePaths(write(bad)); err == nil {
			t.Errorf("accepted %q", bad)
		}
	}
}

func TestArgs(t *testing.T) {
	want := []string{"--socket-path=/s", "--shared-dir=/d", "--readonly", "--cache=auto"}
	if got := Args("/s", "/d"); !slices.Equal(got, want) {
		t.Fatal(got)
	}
}
