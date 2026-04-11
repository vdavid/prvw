package checks

import (
	"os"
	"path/filepath"
)

// RunWebsiteESLint runs ESLint on the website.
func RunWebsiteESLint(ctx *CheckContext) (CheckResult, error) {
	dir := filepath.Join(ctx.RootDir, "apps", "website")

	// Skip if directory doesn't exist yet
	if _, err := os.Stat(dir); os.IsNotExist(err) {
		return Skipped("apps/website/ not found"), nil
	}

	return runESLintCheck(ctx, dir, []string{"*.ts", "*.astro", "*.js"}, true)
}
