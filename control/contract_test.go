package control

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// docs/contracts.md: the control channel's framing and the stats request.

func contractFile(t *testing.T, name string) string {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "testdata", "contract", name))
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

func TestStatsContract(t *testing.T) {
	req, err := StatsRequest(contractFile(t, "meminfo.txt"), contractFile(t, "pressure.txt"))
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := json.Marshal(req)
	if want := strings.TrimSpace(contractFile(t, "stats-request.json")); string(raw) != want {
		t.Fatalf("stats request %s, want %s", raw, want)
	}

	parsed, bad := ReadRequest(bufio.NewReader(strings.NewReader(contractFile(t, "stats-request.json"))))
	if bad != nil || parsed.Op != "stats" {
		t.Fatalf("%+v, %+v", parsed, bad)
	}
	s, bad := parsed.Stats(time.Now())
	if bad != nil || s.MemAvailableKiB != 812345 || s.MemTotalKiB != 8388608 || s.PSISomeAvg10 != 7.25 || s.PSIFullAvg10 != 0.5 {
		t.Fatalf("%+v, %+v", s, bad)
	}
	// Integral PSI averages are numbers too.
	parsed, _ = ReadRequest(bufio.NewReader(strings.NewReader(`{"op":"stats","memAvailableKiB":1,"memTotalKiB":2,"psiSomeAvg10":0.0,"psiFullAvg10":3}` + "\n")))
	if _, bad := parsed.Stats(time.Now()); bad != nil {
		t.Fatal(bad)
	}
}

func TestReplyContract(t *testing.T) {
	for fixture, reply := range map[string]Reply{"reply-ok.json": {OK: true}, "reply-error.json": {Error: "invalid stats"}} {
		raw, _ := json.Marshal(reply)
		if want := strings.TrimSpace(contractFile(t, fixture)); string(raw) != want {
			t.Errorf("%s: %s, want %s", fixture, raw, want)
		}
	}
}
