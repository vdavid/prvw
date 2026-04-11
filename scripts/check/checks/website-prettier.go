package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// RunWebsitePrettier runs Prettier formatting check/fix for the website.
func RunWebsitePrettier(ctx *CheckContext) (CheckResult, error) {
	websiteDir := filepath.Join(ctx.RootDir, "apps", "website")

	// Skip if directory doesn't exist yet
	if _, err := os.Stat(websiteDir); os.IsNotExist(err) {
		return Skipped("apps/website/ not found"), nil
	}

	// Count formattable files
	findCmd := exec.Command("find", "src", "-type", "f", "(", "-name", "*.ts", "-o", "-name", "*.astro", "-o", "-name", "*.js", "-o", "-name", "*.css", "-o", "-name", "*.json", ")")
	findCmd.Dir = websiteDir
	findOutput, _ := RunCommand(findCmd, true)
	fileCount := 0
	if strings.TrimSpace(findOutput) != "" {
		fileCount = len(strings.Split(strings.TrimSpace(findOutput), "\n"))
	}

	if ctx.CI {
		cmd := exec.Command("pnpm", "exec", "prettier", "--check", "src/")
		cmd.Dir = websiteDir
		output, err := RunCommand(cmd, true)
		if err != nil {
			return CheckResult{}, fmt.Errorf("code is not formatted, run pnpm exec prettier --write src/ locally\n%s", indentOutput(output))
		}
		result := Success(fmt.Sprintf("%d %s already formatted", fileCount, Pluralize(fileCount, "file", "files")))
		result.Total = fileCount
		result.Issues = 0
		result.Changes = 0
		return result, nil
	}

	// Non-CI: check first, then format if needed
	checkCmd := exec.Command("pnpm", "exec", "prettier", "--check", "src/")
	checkCmd.Dir = websiteDir
	_, checkErr := RunCommand(checkCmd, true)

	if checkErr != nil {
		fmtCmd := exec.Command("pnpm", "exec", "prettier", "--write", "src/")
		fmtCmd.Dir = websiteDir
		output, err := RunCommand(fmtCmd, true)
		if err != nil {
			return CheckResult{}, fmt.Errorf("prettier formatting failed\n%s", indentOutput(output))
		}
		result := SuccessWithChanges("Formatted files in src/")
		result.Total = fileCount
		return result, nil
	}

	result := Success(fmt.Sprintf("%d %s already formatted", fileCount, Pluralize(fileCount, "file", "files")))
	result.Total = fileCount
	result.Issues = 0
	result.Changes = 0
	return result, nil
}
