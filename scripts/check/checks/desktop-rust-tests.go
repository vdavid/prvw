package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
)

// RunCargoTest runs Rust tests using cargo-nextest.
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

	cmd := exec.Command("cargo", "nextest", "run")
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
