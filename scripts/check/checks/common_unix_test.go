//go:build !windows

package checks

import (
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
)

// TestKillAllProcessesStopsTheWholeTree pins the semantic every check leans on:
// stopping a check stops what the check spawned. `cargo` spawns `rustc`, and a
// grandchild that survives a cancelled run wedges the next one.
func TestKillAllProcessesStopsTheWholeTree(t *testing.T) {
	pidFile := filepath.Join(t.TempDir(), "grandchild.pid")

	// The child shell spawns a grandchild and then waits, so only a signal
	// aimed at the whole process group reaches the grandchild.
	cmd := exec.Command("sh", "-c", "sh -c 'echo $$ > "+pidFile+"; sleep 30' & sleep 30")

	finished := make(chan struct{})
	go func() {
		_, _ = RunCommand(cmd, true)
		close(finished)
	}()

	grandchild := waitForGrandchild(t, pidFile)
	waitUntilTracked(t, cmd)

	KillAllProcesses()

	select {
	case <-finished:
	case <-time.After(10 * time.Second):
		t.Fatal("the child was still running 10s after KillAllProcesses")
	}

	deadline := time.Now().Add(10 * time.Second)
	for syscall.Kill(grandchild, 0) == nil {
		if time.Now().After(deadline) {
			_ = syscall.Kill(grandchild, syscall.SIGKILL)
			t.Fatalf("grandchild %d outlived KillAllProcesses", grandchild)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// waitForGrandchild blocks until the grandchild has written its PID, which also
// means the whole tree is up.
func waitForGrandchild(t *testing.T, pidFile string) int {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if content, err := os.ReadFile(pidFile); err == nil {
			if pid, convErr := strconv.Atoi(strings.TrimSpace(string(content))); convErr == nil {
				return pid
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("the grandchild never wrote its PID")
	return 0
}

// waitUntilTracked blocks until RunCommand has registered cmd, so that
// KillAllProcesses can see it.
func waitUntilTracked(t *testing.T, cmd *exec.Cmd) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		processTracker.mu.Lock()
		_, tracked := processTracker.procs[cmd]
		processTracker.mu.Unlock()
		if tracked {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("RunCommand never registered the command")
}
