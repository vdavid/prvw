package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
)

// RunDesktopSvelteCheck runs svelte-check for type and a11y validation.
func RunDesktopSvelteCheck(ctx *CheckContext) (CheckResult, error) {
	dir := filepath.Join(ctx.RootDir, "apps", "desktop")

	// Skip if desktop app doesn't exist
	if _, err := os.Stat(filepath.Join(dir, "package.json")); os.IsNotExist(err) {
		return Skipped("apps/desktop/package.json not found"), nil
	}

	cmd := exec.Command("pnpm", "check")
	cmd.Dir = dir
	output, err := RunCommand(cmd, true)
	if err != nil {
		return CheckResult{}, fmt.Errorf("svelte-check failed\n%s", indentOutput(output))
	}

	// Check for warnings in output
	lower := strings.ToLower(output)
	if strings.Contains(lower, " warning") && !strings.Contains(lower, "0 warnings") {
		return CheckResult{}, fmt.Errorf("svelte-check found warnings\n%s", indentOutput(output))
	}

	// Try to extract file count from "svelte-check found 0 errors and 0 warnings in X files"
	re := regexp.MustCompile(`in (\d+) files?`)
	matches := re.FindStringSubmatch(output)
	if len(matches) > 1 {
		count, _ := strconv.Atoi(matches[1])
		return Success(fmt.Sprintf("%d %s checked, no errors", count, Pluralize(count, "file", "files"))), nil
	}

	return Success("No type errors"), nil
}
