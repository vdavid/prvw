package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
)

// parityEntryLine matches one row of the generated table's "Every entry" section, which is how
// the check knows the generator produced a table rather than an empty shell.
var parityEntryLine = regexp.MustCompile("(?m)^- `[A-Za-z0-9_]+` \"")

// parityDiffLinesShown caps the reported differences. The fix is always "regenerate", so a few
// lines are enough to see what moved.
const parityDiffLinesShown = 3

// RunParityTable keeps `docs/parity.md` honest: it regenerates the platform parity table from
// the registries in `apps/desktop/src/parity/` and compares it against the committed file.
//
// The generator is `cargo xtask parity`, a dependency-free crate that loads the registries with
// `#[path]`. It builds in well under a second, opens no window, and touches no GPU, so this
// check runs anywhere cargo does, including a headless Windows or Linux runner.
func RunParityTable(ctx *CheckContext) (CheckResult, error) {
	if _, err := os.Stat(filepath.Join(ctx.RootDir, "xtask", "Cargo.toml")); os.IsNotExist(err) {
		return Skipped("xtask/Cargo.toml not found"), nil
	}

	generated, err := generateParityTable(ctx.RootDir)
	if err != nil {
		return CheckResult{}, err
	}
	return compareParityTable(ctx, filepath.Join(ctx.RootDir, "docs", "parity.md"), generated)
}

// generateParityTable runs the xtask and returns its stdout, which is the whole document.
func generateParityTable(rootDir string) (string, error) {
	cmd := exec.Command("cargo", "run", "--quiet", "--package", "xtask", "--", "parity")
	cmd.Dir = rootDir
	// A target directory of its own, so generating the table never waits behind the app's build
	// lock while clippy and the tests hold it. xtask has no dependencies, so it stays tiny.
	cmd.Env = append(os.Environ(), "CARGO_TARGET_DIR="+filepath.Join(rootDir, "target", "xtask"))

	// Stdout is the document, so it has to stay clear of anything cargo says on stderr.
	stdout, stderr, err := RunCommandSplit(cmd)
	if err != nil {
		return "", fmt.Errorf("generating the parity table failed\n%s", indentOutput(stderr))
	}
	return stdout, nil
}

// compareParityTable is the half that decides. In CI a difference is a failure, because the
// committed file is what makes a parity change visible in review. Locally it rewrites the file,
// the way the formatters do.
func compareParityTable(ctx *CheckContext, path string, generated string) (CheckResult, error) {
	entries := len(parityEntryLine.FindAllString(generated, -1))
	if entries == 0 {
		return CheckResult{}, fmt.Errorf("the generator produced no entries, so %s would document nothing", filepath.Base(path))
	}

	committed, readErr := os.ReadFile(path)
	if readErr != nil && !os.IsNotExist(readErr) {
		return CheckResult{}, fmt.Errorf("failed to read %s: %w", path, readErr)
	}
	if readErr == nil && string(committed) == generated {
		result := Success(fmt.Sprintf("%d %s, %s is current", entries, Pluralize(entries, "entry", "entries"), filepath.Base(path)))
		result.Total = entries
		result.Issues = 0
		result.Changes = 0
		return result, nil
	}

	if ctx.CI {
		if os.IsNotExist(readErr) {
			return CheckResult{}, fmt.Errorf("%s is missing, generate it with `./scripts/check.sh --check parity` and commit it", filepath.Base(path))
		}
		return CheckResult{}, fmt.Errorf(
			"%s no longer matches the registries. Run `./scripts/check.sh --check parity` locally and commit the result.\n%s",
			filepath.Base(path), generatedFileDiff(string(committed), generated))
	}

	if err := os.WriteFile(path, []byte(generated), 0644); err != nil {
		return CheckResult{}, fmt.Errorf("failed to write %s: %w", path, err)
	}
	result := SuccessWithChanges(fmt.Sprintf("Regenerated %s (%d %s)", filepath.Base(path), entries, Pluralize(entries, "entry", "entries")))
	result.Total = entries
	result.Issues = 1
	result.Changes = 1
	return result, nil
}

// generatedFileDiff names the first few lines where a committed generated file and a freshly
// generated one disagree, quoted so trailing whitespace and empty lines are unambiguous. Shared
// with the Windows installer check, which regenerates its NSIS include the same way.
func generatedFileDiff(committed, generated string) string {
	committedLines := strings.Split(committed, "\n")
	generatedLines := strings.Split(generated, "\n")

	var sb strings.Builder
	shown := 0
	for i := range max(len(committedLines), len(generatedLines)) {
		before, after := generatedLineAt(committedLines, i), generatedLineAt(generatedLines, i)
		if before == after {
			continue
		}
		if shown == parityDiffLinesShown {
			sb.WriteString("  ...and more further down\n")
			break
		}
		fmt.Fprintf(&sb, "  line %d\n    committed: %s\n    generated: %s\n", i+1, before, after)
		shown++
	}
	return strings.TrimRight(sb.String(), "\n")
}

// generatedLineAt quotes one line, or says the file ended before it.
func generatedLineAt(lines []string, i int) string {
	if i >= len(lines) {
		return "(end of file)"
	}
	return fmt.Sprintf("%q", lines[i])
}
