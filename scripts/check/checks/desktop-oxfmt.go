package checks

import (
	"os"
	"path/filepath"
)

// RunDesktopOxfmt runs oxfmt formatting on the desktop Svelte frontend.
func RunDesktopOxfmt(ctx *CheckContext) (CheckResult, error) {
	dir := filepath.Join(ctx.RootDir, "apps", "desktop")

	// Skip if desktop app doesn't exist
	if _, err := os.Stat(filepath.Join(dir, "package.json")); os.IsNotExist(err) {
		return Skipped("apps/desktop/package.json not found"), nil
	}

	return runOxfmtCheck(ctx, dir, nil)
}
