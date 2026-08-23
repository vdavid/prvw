//go:build !windows

package checks

import (
	"os/exec"
	"syscall"
)

// prepareProcessGroup gives the child its own process group. That shields it
// from the Ctrl+C the terminal sends to the runner's own group, and it gives
// the runner one handle for the whole tree. Call before Start.
func prepareProcessGroup(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
}

// trackProcessGroup completes group setup once the child is running. The group
// exists from the moment the child does, so there is nothing to do.
func trackProcessGroup(*exec.Cmd) {}

// killProcessGroup sends SIGTERM to the child's entire process group, so a
// cancelled `cargo` takes its `rustc` children down with it.
func killProcessGroup(cmd *exec.Cmd) {
	if cmd.Process == nil {
		return
	}
	// A negative PID addresses the group the child leads.
	_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGTERM)
}

// releaseProcessGroup drops what the runner holds for a finished child. Process
// groups are not a resource, so there is nothing to release.
func releaseProcessGroup(*exec.Cmd) {}
