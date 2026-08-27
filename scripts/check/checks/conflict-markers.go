package checks

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// conflictMarkerDirs are the trees worth scanning: source, docs, and the repo root's own
// markdown. Each holds files a person edits by hand, which is where an unresolved merge ends up.
var conflictMarkerDirs = []string{"apps/desktop/src", "apps/website/src", "scripts", "docs", "xtask"}

// conflictMarkerPatterns are the file kinds that carry prose or code we merge by hand. Binary
// and generated files are left out: a marker there is not something a person would fix by hand.
var conflictMarkerPatterns = []string{"*.rs", "*.go", "*.md", "*.ts", "*.tsx", "*.astro", "*.toml", "*.json", "*.css"}

// conflictMarkerGlobs cover the hand-merged files no source tree contains: the repo's own
// markdown, and the manifests that sit one level above `src`. Globbed rather than walked, so
// neither `node_modules` nor `target` is ever descended into.
var conflictMarkerGlobs = []string{
	"*.md",
	"Cargo.toml",
	"package.json",
	"apps/*/Cargo.toml",
	"apps/*/package.json",
	"apps/*/*.md",
	"xtask/Cargo.toml",
}

// conflictMarkerFindingsShown caps the report. The fix is always the same (finish the merge), so
// a handful of locations is enough to act on.
const conflictMarkerFindingsShown = 10

// A conflict marker is these three at the very start of a line. `=======` is deliberately NOT
// among them: it's a legal Markdown setext heading underline, and matching it would fail on
// ordinary documents.
var conflictMarkerPrefixes = []string{"<<<<<<< ", "||||||| ", ">>>>>>> "}

// RunConflictMarkers fails when an unresolved merge conflict marker is committed.
//
// This exists because one got through. A rebase resolution was staged with `git add -A` while a
// doc still carried its markers, the markdown formatter then reflowed `>>>>>>>` into
// `> > > > > > >` (a blockquote, as far as it was concerned), and the result looked like prose in
// every later diff. Nothing else in this runner reads English, so nothing else would ever catch
// it: clippy and the tests only see Rust, and the formatters are happy to format nonsense.
//
// The check is deliberately cheap and dependency-free. It reads line prefixes, so a file that
// merely discusses conflict markers (this one, or a doc about resolving merges) has to write them
// somewhere other than column zero.
func RunConflictMarkers(ctx *CheckContext) (CheckResult, error) {
	var findings []string
	scanned := 0

	roots := append([]string{}, conflictMarkerDirs...)
	for _, dir := range roots {
		files, err := findFiles(filepath.Join(ctx.RootDir, dir), conflictMarkerPatterns...)
		if err != nil {
			return CheckResult{}, err
		}
		for _, rel := range files {
			full := filepath.Join(ctx.RootDir, dir, rel)
			found, err := conflictMarkersIn(full)
			if err != nil {
				return CheckResult{}, err
			}
			scanned++
			for _, line := range found {
				findings = append(findings, fmt.Sprintf("%s/%s:%d", dir, rel, line))
			}
		}
	}

	// Files that sit outside those trees and still get merged by hand: the root's own markdown
	// (`AGENTS.md` is where the marker that prompted this check landed) and every manifest.
	// A manifest is worth naming separately because `apps/desktop/Cargo.toml` is one directory
	// above `apps/desktop/src`, so the walk above never sees it, and a mangled one there took
	// down four checks at once.
	for _, pattern := range conflictMarkerGlobs {
		matches, err := filepath.Glob(filepath.Join(ctx.RootDir, filepath.FromSlash(pattern)))
		if err != nil {
			return CheckResult{}, err
		}
		for _, full := range matches {
			found, err := conflictMarkersIn(full)
			if err != nil {
				return CheckResult{}, err
			}
			scanned++
			rel, relErr := filepath.Rel(ctx.RootDir, full)
			if relErr != nil {
				rel = filepath.Base(full)
			}
			for _, line := range found {
				findings = append(findings, fmt.Sprintf("%s:%d", filepath.ToSlash(rel), line))
			}
		}
	}

	if len(findings) > 0 {
		shown := findings
		if len(shown) > conflictMarkerFindingsShown {
			shown = shown[:conflictMarkerFindingsShown]
		}
		message := fmt.Sprintf("unresolved merge conflict markers in %d place(s):\n%s",
			len(findings), indentOutput(strings.Join(shown, "\n")))
		if len(findings) > len(shown) {
			message += fmt.Sprintf("\n  ... and %d more", len(findings)-len(shown))
		}
		return CheckResult{}, fmt.Errorf("%s", message)
	}

	return Success(fmt.Sprintf("%d files checked, no conflict markers", scanned)), nil
}

// conflictMarkersIn returns the 1-based line numbers carrying a marker at column zero.
func conflictMarkersIn(path string) ([]int, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer func() { _ = file.Close() }()

	var lines []int
	scanner := bufio.NewScanner(file)
	// A minified asset or a long data line shouldn't error the check out.
	scanner.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	lineNumber := 0
	for scanner.Scan() {
		lineNumber++
		text := scanner.Text()
		for _, prefix := range conflictMarkerPrefixes {
			if strings.HasPrefix(text, prefix) {
				lines = append(lines, lineNumber)
				break
			}
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading %s: %w", path, err)
	}
	return lines, nil
}
