package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
)

// RunClippy runs Clippy linter with auto-fix.
func RunClippy(ctx *CheckContext) (CheckResult, error) {
	rustDir := filepath.Join(ctx.RootDir, "apps", "desktop", "src-tauri")

	// Skip if Cargo.toml doesn't exist yet
	if _, err := os.Stat(filepath.Join(rustDir, "Cargo.toml")); os.IsNotExist(err) {
		return Skipped("apps/desktop/src-tauri/Cargo.toml not found"), nil
	}

	// In local mode, first run with --fix to auto-fix what we can
	if !ctx.CI {
		fixCmd := exec.Command("cargo", "clippy", "--all-targets", "--fix", "--allow-dirty", "--allow-staged")
		fixCmd.Dir = rustDir
		_, _ = RunCommand(fixCmd, true) // Ignore errors, we'll catch them in the check run
	}

	// Run clippy WITHOUT --fix to check for remaining issues (--fix ignores -D warnings)
	cmd := exec.Command("cargo", "clippy", "--all-targets", "--", "-D", "warnings")
	cmd.Dir = rustDir
	output, err := RunCommand(cmd, true)
	if err != nil {
		if ctx.CI {
			return CheckResult{}, fmt.Errorf("clippy errors found, run the check script locally\n%s", indentOutput(output))
		}
		return CheckResult{}, fmt.Errorf("clippy found unfixable issues\n%s", indentOutput(output))
	}

	// Try to extract "Compiling X crates" from output
	re := regexp.MustCompile(`Compiling (\d+) crates?`)
	matches := re.FindStringSubmatch(output)
	if len(matches) > 1 {
		count, _ := strconv.Atoi(matches[1])
		result := Success(fmt.Sprintf("Checked %d %s, no warnings", count, Pluralize(count, "crate", "crates")))
		result.Total = count
		return result, nil
	}

	// Fallback: count "Checking" lines
	re2 := regexp.MustCompile(`(?m)^\s*Checking`)
	checkingMatches := re2.FindAllString(output, -1)
	if len(checkingMatches) > 0 {
		count := len(checkingMatches)
		result := Success(fmt.Sprintf("Checked %d %s, no warnings", count, Pluralize(count, "crate", "crates")))
		result.Total = count
		return result, nil
	}

	return Success("No warnings"), nil
}
