// Package hostutil holds the host probes and file helpers vmkit's
// controllers share: accounts, systemd units, load, meminfo and atomic
// writes.
package hostutil

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// Account is a user and the groups it runs with.
type Account struct {
	UID, GID int
	Groups   []uint32
}

// LookupAccount resolves a user name.
func LookupAccount(name string) (Account, error) {
	u, err := user.Lookup(name)
	if err != nil {
		return Account{}, err
	}
	uid, err := strconv.Atoi(u.Uid)
	if err != nil {
		return Account{}, err
	}
	gid, err := strconv.Atoi(u.Gid)
	if err != nil {
		return Account{}, err
	}
	a := Account{UID: uid, GID: gid}
	ids, err := u.GroupIds()
	if err != nil {
		return a, err
	}
	for _, id := range ids {
		if n, err := strconv.Atoi(id); err == nil {
			a.Groups = append(a.Groups, uint32(n))
		}
	}
	return a, nil
}

// Credential runs a child as the account, killed when its parent dies.
func (a Account) Credential() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{Credential: &syscall.Credential{Uid: uint32(a.UID), Gid: uint32(a.GID), Groups: a.Groups}, Pdeathsig: syscall.SIGTERM}
}

// UnitRunning reports whether a unit is up or has a start job queued.
func UnitRunning(ctx context.Context, unit string) (bool, error) {
	ctx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, "systemctl", "show", "--property=ActiveState", "--value", unit).Output()
	if err != nil {
		return true, err
	}
	switch strings.TrimSpace(string(out)) {
	case "active", "activating", "deactivating":
		return true, nil
	}
	job, err := exec.CommandContext(ctx, "systemctl", "show", "--property=Job", "--value", unit).Output()
	if err != nil {
		return true, err
	}
	value := strings.TrimSpace(string(job))
	return value != "" && value != "0" && value != "[0 /]", nil
}

// Load1 returns the 1-minute load average.
func Load1() (float64, error) {
	raw, err := os.ReadFile("/proc/loadavg")
	if err != nil {
		return 0, err
	}
	fields := strings.Fields(string(raw))
	if len(fields) == 0 {
		return 0, errors.New("empty /proc/loadavg")
	}
	return strconv.ParseFloat(fields[0], 64)
}

// MeminfoKiB returns the fields of /proc/meminfo, in KiB.
func MeminfoKiB() (map[string]int64, error) {
	f, err := os.Open("/proc/meminfo")
	if err != nil {
		return nil, err
	}
	defer f.Close()
	mem := map[string]int64{}
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		fields := strings.Fields(sc.Text())
		if len(fields) >= 2 {
			kib, _ := strconv.ParseInt(fields[1], 10, 64)
			mem[strings.TrimSuffix(fields[0], ":")] = kib
		}
	}
	return mem, sc.Err()
}

// ReadBounded reads a file of at most limit bytes.
func ReadBounded(path string, limit int64) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	b, err := io.ReadAll(io.LimitReader(f, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(b)) > limit {
		return nil, errors.New("file exceeds size limit")
	}
	return b, nil
}

// AtomicWrite replaces path with data, durably.
func AtomicWrite(path string, data []byte, mode os.FileMode) error {
	f, err := os.CreateTemp(filepath.Dir(path), "."+filepath.Base(path)+"-*")
	if err != nil {
		return err
	}
	tmp := f.Name()
	defer os.Remove(tmp)
	if err = f.Chmod(mode); err == nil {
		_, err = f.Write(data)
	}
	if err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	if err = os.Rename(tmp, path); err != nil {
		return err
	}
	dir, err := os.Open(filepath.Dir(path))
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}

// WaitForSocket waits for a Unix socket to appear at path.
func WaitForSocket(path string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for {
		if fi, err := os.Stat(path); err == nil && fi.Mode()&os.ModeSocket != 0 {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("%s did not appear", path)
		}
		time.Sleep(20 * time.Millisecond)
	}
}
