package checks

import (
	"fmt"
	"os/exec"
	"path/filepath"
	"strings"
)

// RunStaticcheck runs staticcheck for static analysis.
func RunStaticcheck(ctx *CheckContext) (CheckResult, error) {
	// A pseudo-version for now: Go 1.27.2 (`.mise.toml`) changed the export data format, and the
	// first tag that reads it, 2026.2.2 (v0.8.2), was inside the three-day window when 1.27.2 had to
	// land. This 2026-10-09 commit reads it too. Renovate moves it to a tag, together with Go.
	staticcheckBin, err := EnsureGoTool(ctx.RootDir, "staticcheck", "honnef.co/go/tools/cmd/staticcheck@v0.7.0-0.dev.0.20261009230814-452d5bb86b45")
	if err != nil {
		return CheckResult{}, err
	}

	allModules, err := FindAllGoModules(ctx.RootDir)
	if err != nil {
		return CheckResult{}, fmt.Errorf("failed to find Go modules: %w", err)
	}

	var allIssues []string
	pkgCount := 0

	for goDir, modules := range allModules {
		baseDir := filepath.Join(ctx.RootDir, goDir)
		for _, mod := range modules {
			modDir := filepath.Join(baseDir, mod)
			modLabel := filepath.Join(goDir, mod)

			// Count packages
			listCmd := exec.Command("go", "list", "./...")
			listCmd.Dir = modDir
			listOutput, _ := RunCommand(listCmd, true)
			if strings.TrimSpace(listOutput) != "" {
				pkgCount += len(strings.Split(strings.TrimSpace(listOutput), "\n"))
			}

			cmd := exec.Command(staticcheckBin, "./...")
			cmd.Dir = modDir
			output, err := RunCommand(cmd, true)
			if err != nil {
				issueText := strings.TrimSpace(output)
				if issueText == "" {
					issueText = err.Error()
				}
				allIssues = append(allIssues, fmt.Sprintf("[%s]\n%s", modLabel, issueText))
			}
		}
	}

	if len(allIssues) > 0 {
		return CheckResult{}, fmt.Errorf("staticcheck found issues\n%s", indentOutput(strings.Join(allIssues, "\n")))
	}

	if pkgCount > 0 {
		result := Success(fmt.Sprintf("%d %s checked, no issues", pkgCount, Pluralize(pkgCount, "package", "packages")))
		result.Total = pkgCount
		return result, nil
	}
	return Success("No issues found"), nil
}
