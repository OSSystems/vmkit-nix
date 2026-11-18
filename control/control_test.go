package control

import (
	"bufio"
	"encoding/json"
	"net"
	"strings"
	"testing"
	"time"
)

func TestStatsRequest(t *testing.T) {
	meminfo := "MemTotal:        2014532 kB\nMemFree:          100000 kB\nMemAvailable:     812345 kB\n"
	pressure := "some avg10=7.25 avg60=1.00 avg300=0.10 total=123\nfull avg10=0.50 avg60=0.00 avg300=0.00 total=4\n"
	req, err := StatsRequest(meminfo, pressure)
	if err != nil {
		t.Fatal(err)
	}
	if req.Op != "stats" || *req.MemTotalKiB != 2014532 || *req.MemAvailableKiB != 812345 || *req.PSISomeAvg10 != 7.25 || *req.PSIFullAvg10 != 0.5 {
		t.Fatalf("stats = %+v", req)
	}
	req, err = StatsRequest(meminfo, "")
	if err != nil || *req.PSISomeAvg10 != 0 {
		t.Fatalf("without PSI: %+v, %v", req, err)
	}
	if _, err := StatsRequest("MemFree: 1 kB\n", pressure); err == nil {
		t.Fatal("stats without MemAvailable")
	}
}

func TestRequestStats(t *testing.T) {
	now := time.Now()
	read := func(line string) (Request, *Reply) { return ReadRequest(bufio.NewReader(strings.NewReader(line))) }
	if _, bad := read("not json\n"); bad == nil || bad.Error != "invalid JSON" {
		t.Fatalf("garbage: %+v", bad)
	}
	if _, bad := read(""); bad == nil {
		t.Fatal("empty request accepted")
	}
	req, bad := read(`{"op":"stats","memAvailableKiB":5}`)
	if bad != nil {
		t.Fatal(bad)
	}
	if _, bad := req.Stats(now); bad == nil || bad.Error != "incomplete stats" {
		t.Fatalf("incomplete: %+v", bad)
	}
	req, _ = read(`{"op":"stats","memAvailableKiB":5,"memTotalKiB":4,"psiSomeAvg10":0,"psiFullAvg10":0}`)
	if _, bad := req.Stats(now); bad == nil || bad.Error != "invalid stats" {
		t.Fatalf("available above total: %+v", bad)
	}
	req, _ = read(`{"op":"stats","memAvailableKiB":524288,"memTotalKiB":2097152,"psiSomeAvg10":12,"psiFullAvg10":1}`)
	s, bad := req.Stats(now)
	if bad != nil || !s.Pressured() || !s.ReceivedAt.Equal(now) {
		t.Fatalf("stats %+v, %+v", s, bad)
	}
}

// A caller's reply embeds Reply and adds its own fields.
func TestAnswerWritesCallerReplies(t *testing.T) {
	type jitReply struct {
		Reply
		JITConfig string `json:"jitconfig,omitempty"`
	}
	server, client := net.Pipe()
	delivered := make(chan bool, 1)
	go Answer(server, func(r *bufio.Reader) (any, func(bool)) {
		req, bad := ReadRequest(r)
		if bad != nil {
			return bad, nil
		}
		return jitReply{JITConfig: "cfg-" + req.Op}, func(ok bool) { delivered <- ok }
	})
	client.Write([]byte(`{"op":"jit"}` + "\n"))
	line, err := ReadLine(bufio.NewReader(client), 4096)
	if err != nil {
		t.Fatal(err)
	}
	var got map[string]any
	if err := json.Unmarshal(line, &got); err != nil || got["jitconfig"] != "cfg-jit" || len(got) != 1 {
		t.Fatalf("reply %s, %v", line, err)
	}
	if !<-delivered {
		t.Fatal("delivery not reported")
	}
}

func TestReadLineLimit(t *testing.T) {
	r := bufio.NewReaderSize(strings.NewReader(strings.Repeat("x", 100)+"\n"), 16)
	if _, err := ReadLine(r, 50); err == nil {
		t.Fatal("over-long line accepted")
	}
}
