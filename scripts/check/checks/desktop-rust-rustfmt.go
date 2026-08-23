package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// RunRustfmt formats Rust code across the workspace, so a second crate (`xtask`) is
// formatted by the same run as the app.
func RunRustfmt(ctx *CheckContext) (CheckResult, error) {
	rustDir := filepath.Join(ctx.RootDir, "apps", "desktop")

	// Skip if Cargo.toml doesn't exist yet
	if _, err := os.Stat(filepath.Join(rustDir, "Cargo.toml")); os.IsNotExist(err) {
		return Skipped("apps/desktop/Cargo.toml not found"), nil
	}

	// Count .rs files for the message
	fileCount := countFiles(filepath.Join(rustDir, "src"), "*.rs") + countFiles(filepath.Join(ctx.RootDir, "xtask", "src"), "*.rs")

	// Check which files need formatting (--files-with-diff lists them)
	checkCmd := exec.Command("cargo", "fmt", "--all", "--", "--check", "--files-with-diff")
	checkCmd.Dir = rustDir
	checkOutput, checkErr := RunCommand(checkCmd, true)

	// Parse files that need formatting
	var needsFormat []string
	if strings.TrimSpace(checkOutput) != "" {
		for line := range strings.SplitSeq(strings.TrimSpace(checkOutput), "\n") {
			if strings.HasSuffix(line, ".rs") {
				needsFormat = append(needsFormat, line)
			}
		}
	}

	if ctx.CI {
		if checkErr != nil || len(needsFormat) > 0 {
			return CheckResult{}, fmt.Errorf("code is not formatted, run cargo fmt locally\n%s", indentOutput(checkOutput))
		}
		result := Success(fmt.Sprintf("%d %s already formatted", fileCount, Pluralize(fileCount, "file", "files")))
		result.Total = fileCount
		result.Issues = 0
		result.Changes = 0
		return result, nil
	}

	// Non-CI mode: format if needed
	if len(needsFormat) > 0 {
		fmtCmd := exec.Command("cargo", "fmt", "--all")
		fmtCmd.Dir = rustDir
		output, err := RunCommand(fmtCmd, true)
		if err != nil {
			return CheckResult{}, fmt.Errorf("rust formatting failed\n%s", indentOutput(output))
		}
		result := SuccessWithChanges(fmt.Sprintf("Formatted %d of %d %s", len(needsFormat), fileCount, Pluralize(fileCount, "file", "files")))
		result.Total = fileCount
		result.Issues = len(needsFormat)
		result.Changes = len(needsFormat)
		return result, nil
	}

	result := Success(fmt.Sprintf("%d %s already formatted", fileCount, Pluralize(fileCount, "file", "files")))
	result.Total = fileCount
	result.Issues = 0
	result.Changes = 0
	return result, nil
}
