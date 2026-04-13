package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// RunDesktopSvelteStylelint validates CSS in the desktop Svelte frontend.
func RunDesktopSvelteStylelint(ctx *CheckContext) (CheckResult, error) {
	dir := filepath.Join(ctx.RootDir, "apps", "desktop")

	// Skip if desktop app doesn't exist
	if _, err := os.Stat(filepath.Join(dir, "package.json")); os.IsNotExist(err) {
		return Skipped("apps/desktop/package.json not found"), nil
	}

	// Count CSS files (standalone + embedded in Svelte)
	findCmd := exec.Command("find", "src", "-type", "f", "(", "-name", "*.css", "-o", "-name", "*.svelte", ")")
	findCmd.Dir = dir
	findOutput, _ := RunCommand(findCmd, true)
	fileCount := 0
	if strings.TrimSpace(findOutput) != "" {
		fileCount = len(strings.Split(strings.TrimSpace(findOutput), "\n"))
	}

	var cmd *exec.Cmd
	if ctx.CI {
		cmd = exec.Command("pnpm", "stylelint")
	} else {
		cmd = exec.Command("pnpm", "stylelint:fix")
	}
	cmd.Dir = dir
	output, err := RunCommand(cmd, true)
	if err != nil {
		if ctx.CI {
			return CheckResult{}, fmt.Errorf("CSS lint errors found, run pnpm stylelint:fix locally\n%s", indentOutput(output))
		}
		return CheckResult{}, fmt.Errorf("stylelint found unfixable errors\n%s", indentOutput(output))
	}

	if fileCount > 0 {
		result := Success(fmt.Sprintf("%d CSS %s valid", fileCount, Pluralize(fileCount, "file", "files")))
		result.Total = fileCount
		return result, nil
	}
	return Success("All CSS valid"), nil
}
