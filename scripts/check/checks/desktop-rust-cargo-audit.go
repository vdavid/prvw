package checks

import (
	"encoding/json"
	"fmt"
	"os/exec"
	"sort"
	"strings"
	"time"
)

// cargoAuditReport is the top-level JSON output from `cargo audit --json`.
type cargoAuditReport struct {
	Lockfile struct {
		DependencyCount int `json:"dependency-count"`
	} `json:"lockfile"`
	Vulnerabilities struct {
		Count int                   `json:"count"`
		List  []cargoAuditVulnEntry `json:"list"`
	} `json:"vulnerabilities"`
	Warnings map[string][]cargoAuditWarningEntry `json:"warnings"`
}

type cargoAuditVulnEntry struct {
	Advisory cargoAuditAdvisory `json:"advisory"`
	Versions struct {
		Patched []string `json:"patched"`
	} `json:"versions"`
	Package cargoAuditPackage `json:"package"`
}

type cargoAuditWarningEntry struct {
	Advisory cargoAuditAdvisory `json:"advisory"`
	Package  cargoAuditPackage  `json:"package"`
}

type cargoAuditAdvisory struct {
	ID    string `json:"id"`
	Title string `json:"title"`
}

type cargoAuditPackage struct {
	Name    string `json:"name"`
	Version string `json:"version"`
}

// Advisories we've read and accepted, each for a transitive dependency we can't move ourselves.
// Every entry says why it doesn't reach Prvw and when to drop it. Re-read them when the crate
// that pulls it in ships a new major.
var cargoAuditIgnoredAdvisories = []string{
	"RUSTSEC-2024-0436", // paste unmaintained: a proc-macro, so it runs at build time only (via `image` → `exr` → `pulp`, and `image` → `ravif` → `rav1e`). Remove when `image` drops it.
	"RUSTSEC-2026-0192", // ttf-parser unmaintained (successor is `skrifa`): via `glyphon` → `cosmic-text` → `fontdb`, plus winit's Linux-only `sctk-adwaita`. It parses system fonts only, never a font from an opened file. Remove when `glyphon` moves off it.
	"RUSTSEC-2026-0194", // quick-xml 0.37 quadratic attribute check: only via `little_exif`, a DEV-dependency that writes EXIF into test fixtures we generate ourselves; not in the shipped app. `little_exif` 0.6.23 is its latest and still requires ^0.37. Remove when it accepts >=0.41.
	"RUSTSEC-2026-0195", // quick-xml 0.37 NsReader memory DoS: same dev-only `little_exif` path and removal condition as -0194.
	"RUSTSEC-2026-0253", // lru 0.16 unsound `pop()` when a KEY's `Drop` panics under `catch_unwind`: via `glyphon`'s glyph cache, whose keys are plain data with no custom `Drop`, and Prvw never catches unwinds around text rendering. `glyphon` 0.12.0 is its latest and still requires ^0.16. Remove when it moves to lru >=0.18.2.
}

// buildCargoAuditCmd constructs the cargo audit command with --json and all ignores.
func buildCargoAuditCmd() *exec.Cmd {
	args := []string{"audit", "--json"}
	for _, id := range cargoAuditIgnoredAdvisories {
		args = append(args, "--ignore", id)
	}
	return exec.Command("cargo", args...)
}

// cargoAuditAdvisories turns the report's vulnerabilities into advisories for the fix-age split.
// cargo-audit's `patched` is a requirement, not a version, so the fix is left for OSV to name.
func cargoAuditAdvisories(report cargoAuditReport) []vulnAdvisory {
	var advisories []vulnAdvisory
	for _, v := range report.Vulnerabilities.List {
		advisories = append(advisories, vulnAdvisory{
			ID: v.Advisory.ID, Summary: v.Advisory.Title, Ecosystem: "crates.io",
			Module: v.Package.Name, Version: v.Package.Version,
		})
	}
	return advisories
}

// splitCargoAuditAdvisories names each advisory's fix, then applies the shared fix-age split.
func splitCargoAuditAdvisories(
	advisories []vulnAdvisory,
	now time.Time,
	withFix func(vulnAdvisory) vulnAdvisory,
	fixReleased func(vulnAdvisory) (time.Time, bool),
) (blocking, waiting []vulnAdvisory) {
	filled := make([]vulnAdvisory, len(advisories))
	for i, a := range advisories {
		filled[i] = withFix(a)
	}
	return splitByFixAge(filled, now, fixReleased)
}

// formatAuditWarnings is one sorted line per unmaintained or unsound crate, which warn on their own.
func formatAuditWarnings(report cargoAuditReport) []string {
	var lines []string
	for kind, entries := range report.Warnings {
		for _, w := range entries {
			lines = append(lines, fmt.Sprintf("WARN  %s %s, %s: %s (%s)",
				w.Package.Name, w.Package.Version, kind, w.Advisory.Title, w.Advisory.ID))
		}
	}
	sort.Strings(lines)
	return lines
}

// parseAuditJSON extracts and parses the JSON object from cargo-audit output.
// RunCommand concatenates stderr after stdout, so we find the JSON boundaries.
func parseAuditJSON(output string) (cargoAuditReport, error) {
	jsonStart := strings.Index(output, "{")
	jsonEnd := strings.LastIndex(output, "}")
	if jsonStart < 0 || jsonEnd < 0 || jsonEnd < jsonStart {
		return cargoAuditReport{}, fmt.Errorf("no JSON found in cargo-audit output\n%s", indentOutput(output))
	}
	var report cargoAuditReport
	if err := json.Unmarshal([]byte(output[jsonStart:jsonEnd+1]), &report); err != nil {
		return cargoAuditReport{}, fmt.Errorf("failed to parse cargo-audit JSON: %w\n%s", err, indentOutput(output))
	}
	return report, nil
}

// RunCargoAudit checks for security vulnerabilities.
func RunCargoAudit(ctx *CheckContext) (CheckResult, error) {
	if !CommandExists("cargo-audit") {
		installCmd := exec.Command("cargo", "install", "cargo-audit", "--version", "0.22.2", "--locked")
		if _, err := RunCommand(installCmd, true); err != nil {
			return CheckResult{}, fmt.Errorf("failed to install cargo-audit: %w", err)
		}
	}

	cmd := buildCargoAuditCmd()
	cmd.Dir = ctx.RootDir
	output, _ := RunCommand(cmd, true)

	report, err := parseAuditJSON(output)
	if err != nil {
		return CheckResult{}, err
	}

	// A vulnerability whose fix is too fresh to adopt warns until it's three days past adoptable
	// (`vuln-fix-age.go`); only an advisory that needs no network lookup skips it.
	var blocking, waiting []vulnAdvisory
	if advisories := cargoAuditAdvisories(report); len(advisories) > 0 {
		releases := newCrateReleaseTimes()
		blocking, waiting = splitCargoAuditAdvisories(advisories, time.Now(), releases.osvFix, releases.fixReleased)
	}
	var blockingLines []string
	for _, a := range blocking {
		blockingLines = append(blockingLines, "[Cargo.lock] "+a.describe())
	}
	sort.Strings(blockingLines)
	pending := map[vulnPendingFix]*vulnPendingGroup{}
	for _, a := range waiting {
		addPendingFix(pending, a, "Cargo.lock")
	}
	return vulnVerdict("cargo-audit", blockingLines,
		"Upgrade past the fix, or, if it can't reach what we ship, add it to cargoAuditIgnoredAdvisories with a reason.",
		pending, formatAuditWarnings(report),
		fmt.Sprintf("Scanned %d %s", report.Lockfile.DependencyCount,
			Pluralize(report.Lockfile.DependencyCount, "crate", "crates")))
}
