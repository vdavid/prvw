package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
)

// nextestArgs builds the cargo-nextest command line. `noFailFast` is the raw value of
// PRVW_TEST_NO_FAIL_FAST; any non-empty setting turns nextest's fail-fast off.
//
// nextest cancels the whole run on the first test failure, which is what you want day to day and
// the opposite of what you want on a platform with no track record: one cycle reports one defect
// and leaves the rest of the suite unexecuted. CI's Windows job sets the variable and the other
// two don't. It's read here in the runner rather than in a shell, so it reaches nextest the same
// way through check.sh, check.ps1, and the check.exe the Windows job runs directly.
func nextestArgs(noFailFast string) []string {
	args := []string{"nextest", "run", "--workspace"}
	if noFailFast != "" {
		args = append(args, "--no-fail-fast")
	}
	return args
}

// RunCargoTest runs Rust tests using cargo-nextest, across the whole workspace so the
// `xtask` crate's tests run alongside the app's.
func RunCargoTest(ctx *CheckContext) (CheckResult, error) {
	rustDir := filepath.Join(ctx.RootDir, "apps", "desktop")

	// Skip if Cargo.toml doesn't exist yet
	if _, err := os.Stat(filepath.Join(rustDir, "Cargo.toml")); os.IsNotExist(err) {
		return Skipped("apps/desktop/Cargo.toml not found"), nil
	}

	// Check if cargo-nextest is installed
	if !CommandExists("cargo-nextest") {
		installCmd := exec.Command("cargo", "install", "cargo-nextest", "--locked")
		if _, err := RunCommand(installCmd, true); err != nil {
			return CheckResult{}, fmt.Errorf("failed to install cargo-nextest: %w", err)
		}
	}

	cmd := exec.Command("cargo", nextestArgs(os.Getenv("PRVW_TEST_NO_FAIL_FAST"))...)
	cmd.Dir = rustDir
	output, err := RunCommand(cmd, true)
	if err != nil {
		return CheckResult{}, fmt.Errorf("rust tests failed\n%s", indentOutput(output))
	}

	// Parse test count from output: "X tests run:"
	re := regexp.MustCompile(`(\d+) tests? run`)
	matches := re.FindStringSubmatch(output)
	if len(matches) > 1 {
		count, _ := strconv.Atoi(matches[1])
		result := Success(fmt.Sprintf("%d %s passed", count, Pluralize(count, "test", "tests")))
		result.Total = count
		return result, nil
	}
	return Success("All tests passed"), nil
}
