package main

import (
	"encoding/csv"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

// projectLogDirName is this repo's subdirectory under the shared check-runner
// data dir, and the ONE token a copy of this runner in another repo has to
// change: the filename and the resolver below are identical everywhere.
const projectLogDirName = "prvw"

const csvFileName = "check-log.csv"

var (
	csvHeader = []string{"timestamp", "app", "check", "duration_s", "result", "total", "issues", "changes", "message"}
	csvMu     sync.Mutex
)

// logPath resolves the log to ~/.local/share/check-runner/<project>/<fileName>,
// honoring $XDG_DATA_HOME. The log lives outside the repo on purpose: it's a
// measurement history spanning worktrees, and a teardown must never take it.
func logPath(fileName string) (string, error) {
	dataHome := os.Getenv("XDG_DATA_HOME")
	if dataHome == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		dataHome = filepath.Join(home, ".local", "share")
	}
	return filepath.Join(dataHome, "check-runner", projectLogDirName, fileName), nil
}

// logCheckStats appends one CSV row to the per-run log with the check result.
func logCheckStats(state *CheckState) {
	csvMu.Lock()
	defer csvMu.Unlock()
	csvPath, err := logPath(csvFileName)
	if err != nil {
		return
	}
	if err := os.MkdirAll(filepath.Dir(csvPath), 0o755); err != nil {
		return
	}

	_, statErr := os.Stat(csvPath)
	isNew := os.IsNotExist(statErr)

	f, err := os.OpenFile(csvPath, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		return
	}
	defer f.Close()

	w := csv.NewWriter(f)
	defer w.Flush()

	if isNew {
		_ = w.Write(csvHeader)
	}

	timestamp := time.Now().Format("2006-01-02 15:04:05")
	app := string(state.Definition.App)
	check := state.Definition.CLIName()
	durationS := fmt.Sprintf("%.3f", state.Duration.Seconds())

	result := "pass"
	message := state.Result.Message
	switch state.Status {
	case StatusFailed:
		result = "fail"
		if state.Error != nil {
			message = state.Error.Error()
		}
	case StatusSkipped:
		result = "skip"
	case StatusBlocked:
		result = "blocked"
		message = "dependency failed"
	}

	// First line only
	if i := strings.IndexByte(message, '\n'); i >= 0 {
		message = message[:i]
	}

	total := formatCount(state.Result.Total)
	issues := formatCount(state.Result.Issues)
	changes := formatCount(state.Result.Changes)

	_ = w.Write([]string{timestamp, app, check, durationS, result, total, issues, changes, message})
}

func formatCount(n int) string {
	if n < 0 {
		return "N/A"
	}
	return fmt.Sprintf("%d", n)
}
