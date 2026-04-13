package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// RunDesktopSveltePrettier runs Prettier formatting check/fix for the desktop Svelte frontend.
func RunDesktopSveltePrettier(ctx *CheckContext) (CheckResult, error) {
	dir := filepath.Join(ctx.RootDir, "apps", "desktop")

	// Skip if desktop app doesn't exist
	if _, err := os.Stat(filepath.Join(dir, "package.json")); os.IsNotExist(err) {
		return Skipped("apps/desktop/package.json not found"), nil
	}

	// Count formattable files
	findArgs := buildFindArgs("src", []string{"*.ts", "*.svelte", "*.js", "*.css", "*.json"})
	findCmd := exec.Command("find", findArgs...)
	findCmd.Dir = dir
	findOutput, _ := RunCommand(findCmd, true)
	fileCount := 0
	if strings.TrimSpace(findOutput) != "" {
		fileCount = len(strings.Split(strings.TrimSpace(findOutput), "\n"))
	}

	if ctx.CI {
		cmd := exec.Command("pnpm", "format:check")
		cmd.Dir = dir
		output, err := RunCommand(cmd, true)
		if err != nil {
			return CheckResult{}, fmt.Errorf("code is not formatted, run pnpm format locally\n%s", indentOutput(output))
		}
		result := Success(fmt.Sprintf("%d %s already formatted", fileCount, Pluralize(fileCount, "file", "files")))
		result.Total = fileCount
		result.Issues = 0
		result.Changes = 0
		return result, nil
	}

	// Non-CI: check first, then format if needed
	checkCmd := exec.Command("pnpm", "format:check")
	checkCmd.Dir = dir
	_, checkErr := RunCommand(checkCmd, true)

	if checkErr != nil {
		fmtCmd := exec.Command("pnpm", "format")
		fmtCmd.Dir = dir
		output, err := RunCommand(fmtCmd, true)
		if err != nil {
			return CheckResult{}, fmt.Errorf("prettier formatting failed\n%s", indentOutput(output))
		}
		result := SuccessWithChanges("Formatted files")
		result.Total = fileCount
		return result, nil
	}

	result := Success(fmt.Sprintf("%d %s already formatted", fileCount, Pluralize(fileCount, "file", "files")))
	result.Total = fileCount
	result.Issues = 0
	result.Changes = 0
	return result, nil
}
