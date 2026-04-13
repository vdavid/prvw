package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
)

// RunDesktopSvelteBuild builds the desktop Svelte frontend.
func RunDesktopSvelteBuild(ctx *CheckContext) (CheckResult, error) {
	dir := filepath.Join(ctx.RootDir, "apps", "desktop")

	// Skip if desktop app doesn't exist
	if _, err := os.Stat(filepath.Join(dir, "package.json")); os.IsNotExist(err) {
		return Skipped("apps/desktop/package.json not found"), nil
	}

	cmd := exec.Command("pnpm", "build")
	cmd.Dir = dir
	output, err := RunCommand(cmd, true)
	if err != nil {
		return CheckResult{}, fmt.Errorf("build failed\n%s", indentOutput(output))
	}

	return Success("Build succeeded"), nil
}
