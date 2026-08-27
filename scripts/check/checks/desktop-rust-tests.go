package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
)

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

	args := []string{"nextest", "run", "--workspace"}
	// nextest cancels the whole run on the first test failure, which is right day to day and
	// wrong when a platform is failing for the first time: one CI cycle then reports one defect
	// and hides every other. Setting PRVW_TEST_NO_FAIL_FAST=1 asks for the full picture instead.
	// Deliberately opt-in and env-driven: a workflow_dispatch run sets it in one `env:` line,
	// and nothing about the default path changes.
	if os.Getenv("PRVW_TEST_NO_FAIL_FAST") != "" {
		args = append(args, "--no-fail-fast")
	}
	cmd := exec.Command("cargo", args...)
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
