//go:build windows

package checks

import (
	"os/exec"
	"sync"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

// Windows has no signal that addresses a process tree, so each child goes into
// its own job object instead. A job holds the child and everything the child
// spawns, and terminating the job terminates all of them at once. That is what
// keeps a cancelled `cargo` from leaving `rustc` children running.
//
// The job is assigned right after Start rather than around a CREATE_SUSPENDED
// start: os/exec closes the child's main thread handle before it returns, so
// nothing supported can resume the child afterwards, and hunting the thread down
// through a Toolhelp snapshot buys nothing real. Assignment lands while the
// child is still loading its image, well before it can spawn anything.
//
// JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE doubles as a backstop: if the runner exits
// without cleaning up, its last handle on the job closes with it and the tree
// goes too.
var jobObjects = struct {
	mu    sync.Mutex
	byCmd map[*exec.Cmd]windows.Handle
}{byCmd: make(map[*exec.Cmd]windows.Handle)}

// prepareProcessGroup puts the child in its own console process group, so a
// Ctrl+C typed at the runner reaches the runner alone and the runner decides
// when the tree dies. Call before Start.
func prepareProcessGroup(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: windows.CREATE_NEW_PROCESS_GROUP}
}

// trackProcessGroup puts the running child into a fresh job object. Every step
// can fail on a locked-down machine; when one does the child simply has no job,
// and killProcessGroup falls back to stopping the child alone.
func trackProcessGroup(cmd *exec.Cmd) {
	if cmd.Process == nil {
		return
	}

	job, err := windows.CreateJobObject(nil, nil)
	if err != nil {
		return
	}

	limits := windows.JOBOBJECT_EXTENDED_LIMIT_INFORMATION{
		BasicLimitInformation: windows.JOBOBJECT_BASIC_LIMIT_INFORMATION{
			LimitFlags: windows.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
		},
	}
	if _, err := windows.SetInformationJobObject(
		job,
		windows.JobObjectExtendedLimitInformation,
		uintptr(unsafe.Pointer(&limits)),
		uint32(unsafe.Sizeof(limits)),
	); err != nil {
		_ = windows.CloseHandle(job)
		return
	}

	process, err := windows.OpenProcess(windows.PROCESS_SET_QUOTA|windows.PROCESS_TERMINATE, false, uint32(cmd.Process.Pid))
	if err != nil {
		_ = windows.CloseHandle(job)
		return
	}
	defer func() { _ = windows.CloseHandle(process) }()

	if err := windows.AssignProcessToJobObject(job, process); err != nil {
		_ = windows.CloseHandle(job)
		return
	}

	jobObjects.mu.Lock()
	jobObjects.byCmd[cmd] = job
	jobObjects.mu.Unlock()
}

// killProcessGroup terminates the child and everything it spawned.
func killProcessGroup(cmd *exec.Cmd) {
	if cmd.Process == nil {
		return
	}

	jobObjects.mu.Lock()
	job, hasJob := jobObjects.byCmd[cmd]
	jobObjects.mu.Unlock()

	if hasJob {
		_ = windows.TerminateJobObject(job, 1)
		return
	}
	_ = cmd.Process.Kill()
}

// releaseProcessGroup closes the job handle of a finished child. The job is
// empty by then, so closing it frees the handle without killing anything.
func releaseProcessGroup(cmd *exec.Cmd) {
	jobObjects.mu.Lock()
	job, hasJob := jobObjects.byCmd[cmd]
	delete(jobObjects.byCmd, cmd)
	jobObjects.mu.Unlock()

	if hasJob {
		_ = windows.CloseHandle(job)
	}
}
