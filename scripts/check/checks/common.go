package checks

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"strings"
	"sync"
)

// App represents the application a check belongs to.
type App string

const (
	AppDesktop App = "desktop"
	AppWebsite App = "website"
	AppScripts App = "scripts"
	AppOther   App = "other"
)

// AppDisplayName returns a human-readable name for an app with icon.
func AppDisplayName(app App) string {
	switch app {
	case AppDesktop:
		return "🖥️  Desktop"
	case AppWebsite:
		return "🌐 Website"
	case AppScripts:
		return "📜 Scripts"
	case AppOther:
		return "📦 Other"
	default:
		return string(app)
	}
}

// ResultCode indicates the outcome of a check.
type ResultCode int

const (
	ResultSuccess ResultCode = iota
	ResultWarning
	ResultSkipped
)

// CheckResult is returned by checks on success.
type CheckResult struct {
	Code        ResultCode
	Message     string
	MadeChanges bool // true if the check modified files (for example, formatted code)
	Total       int  // items checked (-1 = N/A)
	Issues      int  // items needing attention (-1 = N/A)
	Changes     int  // files modified (-1 = N/A)
}

// Success creates a success result with the given message (no changes made).
func Success(message string) CheckResult {
	return CheckResult{Code: ResultSuccess, Message: message, Total: -1, Issues: -1, Changes: -1}
}

// SuccessWithChanges creates a success result indicating files were modified.
func SuccessWithChanges(message string) CheckResult {
	return CheckResult{Code: ResultSuccess, Message: message, MadeChanges: true, Total: -1, Issues: -1, Changes: -1}
}

// Skipped creates a skipped result with the given reason.
func Skipped(reason string) CheckResult {
	return CheckResult{Code: ResultSkipped, Message: reason, Total: -1, Issues: -1, Changes: -1}
}

// CheckContext holds the context for running checks.
type CheckContext struct {
	CI      bool
	Verbose bool
	RootDir string
}

// CheckFunc is the function signature for check implementations.
type CheckFunc func(ctx *CheckContext) (CheckResult, error)

// CheckDefinition defines a check's metadata and implementation.
type CheckDefinition struct {
	ID          string
	Nickname    string // Short alias shown in --help and accepted by --check (if empty, ID is used)
	DisplayName string
	App         App
	Tech        string
	IsSlow      bool
	DependsOn   []string
	Run         CheckFunc
}

// processTracker keeps track of all running child processes so they can be
// killed as a group on Ctrl+C. Each command is grouped for tree-wide killing by
// the per-OS helpers in common_unix.go and common_windows.go, so stopping a
// tracked child stops everything it spawned.
var processTracker = struct {
	mu    sync.Mutex
	procs map[*exec.Cmd]struct{}
}{procs: make(map[*exec.Cmd]struct{})}

// KillAllProcesses stops every tracked child and the whole process tree below it.
func KillAllProcesses() {
	processTracker.mu.Lock()
	defer processTracker.mu.Unlock()
	for cmd := range processTracker.procs {
		killProcessGroup(cmd)
	}
}

// RunCommand executes a command and captures its output, stderr appended to stdout.
// The command is grouped so that all of its descendants can be killed together
// on shutdown.
func RunCommand(cmd *exec.Cmd, captureOutput bool) (string, error) {
	if !captureOutput {
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr
		return "", runTracked(cmd)
	}

	stdout, stderr, err := RunCommandSplit(cmd)
	return stdout + stderr, err
}

// RunCommandSplit runs a command like RunCommand but keeps the two streams apart, for the
// checks that consume stdout as data rather than as a report (the parity table is generated
// this way, and anything cargo says on stderr would corrupt it).
func RunCommandSplit(cmd *exec.Cmd) (string, string, error) {
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := runTracked(cmd)
	return stdout.String(), stderr.String(), err
}

// runTracked starts a command in its own process group, waits for it, and untracks it, so
// Ctrl+C takes down the whole tree it spawned.
func runTracked(cmd *exec.Cmd) error {
	prepareProcessGroup(cmd)

	if err := cmd.Start(); err != nil {
		return err
	}
	trackProcessGroup(cmd)

	processTracker.mu.Lock()
	processTracker.procs[cmd] = struct{}{}
	processTracker.mu.Unlock()

	err := cmd.Wait()

	processTracker.mu.Lock()
	delete(processTracker.procs, cmd)
	processTracker.mu.Unlock()
	releaseProcessGroup(cmd)

	return err
}

// CommandExists checks if a command exists in PATH.
func CommandExists(name string) bool {
	_, err := exec.LookPath(name)
	return err == nil
}

// EnsureGoTool ensures a Go tool is installed and returns the path to the binary.
// If the tool is already in PATH, returns just the name. Otherwise installs it
// and returns the full path to the installed binary.
func EnsureGoTool(name, installPath string) (string, error) {
	if CommandExists(name) {
		return name, nil
	}

	// Get Go's bin directory
	goBin := getGoBinDir()
	if goBin == "" {
		return "", fmt.Errorf("could not determine Go bin directory")
	}

	// Install the tool
	installCmd := exec.Command("go", "install", installPath)
	if _, err := RunCommand(installCmd, true); err != nil {
		return "", fmt.Errorf("failed to install %s: %w", name, err)
	}

	// Return full path to the binary
	return filepath.Join(goBin, name), nil
}

// getGoBinDir returns the directory where go install puts binaries.
func getGoBinDir() string {
	// First check GOBIN
	cmd := exec.Command("go", "env", "GOBIN")
	if output, err := RunCommand(cmd, true); err == nil {
		if bin := strings.TrimSpace(output); bin != "" {
			return bin
		}
	}

	// Fall back to GOPATH/bin
	cmd = exec.Command("go", "env", "GOPATH")
	if output, err := RunCommand(cmd, true); err == nil {
		if gopath := strings.TrimSpace(output); gopath != "" {
			return filepath.Join(gopath, "bin")
		}
	}

	// Last resort: ~/go/bin
	if home, err := os.UserHomeDir(); err == nil {
		return filepath.Join(home, "go", "bin")
	}

	return ""
}

// indentOutput indents each non-empty line of output.
func indentOutput(output string) string {
	lines := strings.Split(output, "\n")
	var result strings.Builder
	for _, line := range lines {
		if strings.TrimSpace(line) != "" {
			result.WriteString("      ")
			result.WriteString(line)
			result.WriteString("\n")
		}
	}
	return result.String()
}

// EnsurePnpmDependencies runs pnpm install to ensure all dependencies are installed.
// Skips the install if pnpm-lock.yaml hasn't changed since the last successful run.
// In CI mode, uses --frozen-lockfile and always runs (never skips).
// Returns true if the install was skipped.
func EnsurePnpmDependencies(ctx *CheckContext) (skipped bool, err error) {
	lockfilePath := filepath.Join(ctx.RootDir, "pnpm-lock.yaml")
	markerPath := filepath.Join(ctx.RootDir, "node_modules", ".pnpm-install-marker")

	if !ctx.CI {
		if lockInfo, lockErr := os.Stat(lockfilePath); lockErr == nil {
			if markerContent, markerErr := os.ReadFile(markerPath); markerErr == nil {
				recorded := string(markerContent)
				current := lockInfo.ModTime().UTC().Format("2006-01-02T15:04:05.000000000Z")
				if recorded == current {
					return true, nil
				}
			}
		}
	}

	// --ignore-scripts: recent pnpm (10.16+, 11.x) errors out on un-approved
	// dependency build scripts (ERR_PNPM_IGNORED_BUILDS). esbuild and sharp ship
	// prebuilt binaries and don't need their postinstall scripts, so skip them.
	// See apps/website/CLAUDE.md.
	args := []string{"install", "--ignore-scripts"}
	if ctx.CI {
		args = append(args, "--frozen-lockfile")
	}

	cmd := exec.Command("pnpm", args...)
	cmd.Dir = ctx.RootDir
	output, err := RunCommand(cmd, true)
	if err != nil {
		return false, fmt.Errorf("pnpm install failed:\n%s", indentOutput(output))
	}

	// Write marker with lockfile's current mtime
	if lockInfo, lockErr := os.Stat(lockfilePath); lockErr == nil {
		mtime := lockInfo.ModTime().UTC().Format("2006-01-02T15:04:05.000000000Z")
		_ = os.WriteFile(markerPath, []byte(mtime), 0644)
	}

	return false, nil
}

// Pluralize returns singular if count is 1, plural otherwise.
func Pluralize(count int, singular, plural string) string {
	if count == 1 {
		return singular
	}
	return plural
}

// runESLintCheck runs ESLint check/fix for a given directory.
// extensions are the file extensions to count (like []string{"*.ts", "*.astro", "*.js"}).
// If requireConfig is true, skips when eslint.config.js is missing.
func runESLintCheck(ctx *CheckContext, dir string, extensions []string, requireConfig bool) (CheckResult, error) {
	if requireConfig {
		if _, err := os.Stat(filepath.Join(dir, "eslint.config.js")); os.IsNotExist(err) {
			return Skipped("no eslint.config.js"), nil
		}
	}

	// Count lintable files
	fileCount := countFiles(filepath.Join(dir, "src"), extensions...)

	var cmd *exec.Cmd
	if ctx.CI {
		cmd = exec.Command("pnpm", "lint")
	} else {
		cmd = exec.Command("pnpm", "lint:fix")
	}
	cmd.Dir = dir
	output, err := RunCommand(cmd, true)
	if err != nil {
		if ctx.CI {
			return CheckResult{}, fmt.Errorf("lint errors found, run pnpm lint:fix locally\n%s", indentOutput(output))
		}
		return CheckResult{}, fmt.Errorf("eslint found unfixable errors\n%s", indentOutput(output))
	}

	if fileCount > 0 {
		result := Success(fmt.Sprintf("%d %s passed", fileCount, Pluralize(fileCount, "file", "files")))
		result.Total = fileCount
		return result, nil
	}
	return Success("All files passed"), nil
}

// runOxfmtCheck runs oxfmt formatting check/fix for a given directory.
// File count is parsed from oxfmt's "Finished in ..." line.
func runOxfmtCheck(ctx *CheckContext, dir string) (CheckResult, error) {
	if ctx.CI {
		checkCmd := exec.Command("pnpm", "exec", "oxfmt", "--check", ".")
		checkCmd.Dir = dir
		checkOutput, err := RunCommand(checkCmd, true)
		fileCount := parseOxfmtFileCount(checkOutput)
		if err != nil {
			return CheckResult{}, fmt.Errorf("code is not formatted, run `pnpm exec oxfmt .` locally\n%s", indentOutput(checkOutput))
		}
		result := Success(fmt.Sprintf("%d %s already formatted", fileCount, Pluralize(fileCount, "file", "files")))
		result.Total = fileCount
		result.Issues = 0
		result.Changes = 0
		return result, nil
	}

	checkCmd := exec.Command("pnpm", "exec", "oxfmt", "--check", ".")
	checkCmd.Dir = dir
	checkOutput, checkErr := RunCommand(checkCmd, true)
	fileCount := parseOxfmtFileCount(checkOutput)

	if checkErr != nil {
		fmtCmd := exec.Command("pnpm", "exec", "oxfmt", ".")
		fmtCmd.Dir = dir
		fmtOutput, err := RunCommand(fmtCmd, true)
		if err != nil {
			return CheckResult{}, fmt.Errorf("oxfmt formatting failed\n%s", indentOutput(fmtOutput))
		}

		var needsFormat int
		for line := range strings.SplitSeq(strings.TrimSpace(checkOutput), "\n") {
			trimmed := strings.TrimSpace(line)
			if trimmed != "" && !strings.HasPrefix(trimmed, "Checking") && !strings.HasPrefix(trimmed, "Finished") && !strings.HasPrefix(trimmed, "Format") {
				needsFormat++
			}
		}

		result := SuccessWithChanges(fmt.Sprintf("Formatted %d of %d %s", needsFormat, fileCount, Pluralize(fileCount, "file", "files")))
		result.Total = fileCount
		result.Issues = needsFormat
		result.Changes = needsFormat
		return result, nil
	}

	result := Success(fmt.Sprintf("%d %s already formatted", fileCount, Pluralize(fileCount, "file", "files")))
	result.Total = fileCount
	result.Issues = 0
	result.Changes = 0
	return result, nil
}

// parseOxfmtFileCount extracts the file count from oxfmt output like "Finished in 150ms on 25 files using 16 threads."
func parseOxfmtFileCount(output string) int {
	for line := range strings.SplitSeq(output, "\n") {
		if strings.HasPrefix(line, "Finished in ") {
			var count int
			if _, err := fmt.Sscanf(line, "Finished in %s on %d files", new(string), &count); err == nil {
				return count
			}
		}
	}
	return 0
}

// GetGoDirectories returns all directories in the repo that contain Go code.
// Each returned path is relative to rootDir.
func GetGoDirectories() []string {
	return []string{
		"scripts",
	}
}

// FindGoModules finds all go.mod files in the given directory and returns
// the directories containing them, relative to rootDir. A go.mod at rootDir
// itself is reported as ".".
func FindGoModules(rootDir string) ([]string, error) {
	files, err := findFiles(rootDir, "go.mod")
	if err != nil {
		return nil, err
	}

	var modules []string
	for _, file := range files {
		modules = append(modules, path.Dir(file))
	}
	return modules, nil
}

// FindAllGoModules finds Go modules across all Go directories in the repo.
// Returns a map of base directory to list of module subdirectories.
func FindAllGoModules(rootDir string) (map[string][]string, error) {
	result := make(map[string][]string)
	for _, goDir := range GetGoDirectories() {
		fullPath := filepath.Join(rootDir, goDir)
		modules, err := FindGoModules(fullPath)
		if err != nil {
			return nil, fmt.Errorf("failed to find modules in %s: %w", goDir, err)
		}
		result[goDir] = modules
	}
	return result, nil
}
