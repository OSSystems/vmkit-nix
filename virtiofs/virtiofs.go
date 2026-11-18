// Package virtiofs shares host directories with guests through virtiofsd,
// and builds a store holding only one Nix closure, so a guest sees its own
// system and nothing else of the host's store.
package virtiofs

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"syscall"

	"github.com/OSSystems/vmkit-nix/hostutil"
)

// Args are virtiofsd's arguments to serve dir read-only at socket.
func Args(socket, dir string) []string {
	return []string{
		"--socket-path=" + socket,
		"--shared-dir=" + dir,
		"--readonly",
		"--cache=auto",
	}
}

// StorePaths reads a closure's store paths, one per line, as
// closureInfo's store-paths file lists them.
func StorePaths(closure string) ([]string, error) {
	raw, err := hostutil.ReadBounded(closure, 16<<20)
	if err != nil {
		return nil, err
	}
	var paths []string
	for _, line := range strings.Split(string(raw), "\n") {
		if line == "" {
			continue
		}
		name, ok := strings.CutPrefix(line, "/nix/store/")
		if !ok || name == "" || name == "." || name == ".." || strings.Contains(name, "/") {
			return nil, fmt.Errorf("not a store path: %q", line)
		}
		paths = append(paths, line)
	}
	return paths, nil
}

// StoreView makes dir a store holding only the closure: each path is
// bind-mounted read-only from the host store. The mounts belong to the
// calling process's mount namespace; RequirePrivateMounts first keeps them
// out of the host's.
func StoreView(closure, dir string) error {
	paths, err := StorePaths(closure)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0755); err != nil {
		return err
	}
	if err := os.Chmod(dir, 0755); err != nil {
		return err
	}
	for _, path := range paths {
		target := filepath.Join(dir, filepath.Base(path))
		fi, err := os.Lstat(path)
		if err != nil {
			return err
		}
		switch {
		case fi.Mode()&fs.ModeSymlink != 0:
			link, err := os.Readlink(path)
			if err != nil {
				return err
			}
			if err := os.Symlink(link, target); err != nil {
				return err
			}
			continue
		case fi.IsDir():
			err = os.Mkdir(target, 0555)
		default:
			err = os.WriteFile(target, nil, 0444)
		}
		if err != nil {
			return err
		}
		if err := syscall.Mount(path, target, "", syscall.MS_BIND, ""); err != nil {
			return fmt.Errorf("binding %s: %w", path, err)
		}
		if err := syscall.Mount("", target, "", syscall.MS_BIND|syscall.MS_REMOUNT|syscall.MS_RDONLY|syscall.MS_NOSUID|syscall.MS_NODEV, ""); err != nil {
			return fmt.Errorf("making %s read-only: %w", path, err)
		}
	}
	return nil
}

// RequirePrivateMounts fails unless the process runs in a mount namespace
// of its own (systemd's PrivateMounts=), so StoreView's mounts go with it.
func RequirePrivateMounts() error {
	self, err := os.Readlink("/proc/self/ns/mnt")
	if err != nil {
		return err
	}
	init, err := os.Readlink("/proc/1/ns/mnt")
	if err != nil {
		return err
	}
	if self == init {
		return errors.New("run in a private mount namespace (PrivateMounts=yes)")
	}
	return nil
}
