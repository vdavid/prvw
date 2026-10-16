package checks

import (
	"fmt"
	"sort"
	"strings"
	"time"
)

// The fix-age policy the vulnerability lanes share (`govulncheck`, `cargo-audit`): an advisory
// warns while its fix is too fresh to adopt, keeps warning for a grace period once it's
// adoptable, and fails after that. Ported from Cmdr's `vuln-fix-age.go`.

// vulnFixAdoptionWindow is how old a fix must be before we adopt it: the same three-day
// window pnpm's `minimum-release-age` and Renovate's `minimumReleaseAge` hold every other
// dependency to. Failing before then would demand an upgrade the policy forbids.
const vulnFixAdoptionWindow = 72 * time.Hour

// vulnUpgradeGrace is how long an advisory keeps warning after its fix becomes adoptable,
// so whoever sees the warning has three days to upgrade before the lane turns red for everyone.
const vulnUpgradeGrace = 72 * time.Hour

// vulnAdvisory is one advisory against one package version we use.
type vulnAdvisory struct {
	ID           string
	Summary      string
	Published    time.Time
	Ecosystem    string // OSV's name: "crates.io"; "" from govulncheck (always Go)
	Module       string // the package; "stdlib" for the Go standard library
	Version      string // the version we use
	FixedVersion string // "" when no fix exists
	FixAdoptable time.Time
	FixDue       time.Time
}

func (a vulnAdvisory) describe() string {
	fix := "no fix available"
	if a.FixedVersion != "" {
		fix = "fixed in " + a.FixedVersion
	}
	return fmt.Sprintf("%s %s@%s, %s: %s", a.ID, a.Module, a.Version, fix, a.Summary)
}

// vulnPendingFix is one not-yet-adoptable upgrade. A Go point release closes a dozen stdlib
// advisories at once, so the warning lists one line per upgrade, not per finding.
type vulnPendingFix struct {
	module, version, fixedVersion string
	adoptable, due                time.Time
}

type vulnPendingGroup struct {
	ids, modules map[string]bool
}

// addPendingFix files a waiting advisory under its upgrade; `where` names the Go module or
// lockfile it was found in.
func addPendingFix(groups map[vulnPendingFix]*vulnPendingGroup, a vulnAdvisory, where string) {
	key := vulnPendingFix{a.Module, a.Version, a.FixedVersion, a.FixAdoptable, a.FixDue}
	g := groups[key]
	if g == nil {
		g = &vulnPendingGroup{ids: map[string]bool{}, modules: map[string]bool{}}
		groups[key] = g
	}
	g.ids[a.ID] = true
	g.modules[where] = true
}

// formatPendingFixes returns one sorted line per pending upgrade, plus how many distinct
// advisories they close.
func formatPendingFixes(groups map[vulnPendingFix]*vulnPendingGroup) ([]string, int) {
	var lines []string
	count := 0
	for key, g := range groups {
		ids, modules := sortedKeys(g.ids), sortedKeys(g.modules)
		count += len(ids)
		lines = append(lines, fmt.Sprintf("%s@%s, fixed in %s, adoptable %s, fails %s: %s (in %s)",
			key.module, key.version, key.fixedVersion,
			key.adoptable.UTC().Format("2006-01-02 15:04 MST"), key.due.UTC().Format("2006-01-02 15:04 MST"),
			strings.Join(ids, ", "), strings.Join(modules, ", ")))
	}
	sort.Strings(lines)
	return lines, count
}

// splitByFixAge sorts advisories into those to fail on and those whose fix is still inside the
// adoption window plus the upgrade grace. A missing fix, or one we can't date, fails: waiting is
// only for a fix that provably exists and is provably not yet due.
func splitByFixAge(
	advisories []vulnAdvisory,
	now time.Time,
	fixReleased func(vulnAdvisory) (time.Time, bool),
) (blocking, waiting []vulnAdvisory) {
	for _, a := range advisories {
		if a.FixedVersion != "" {
			if released, ok := fixReleased(a); ok {
				a.FixAdoptable = released.Add(vulnFixAdoptionWindow)
				a.FixDue = a.FixAdoptable.Add(vulnUpgradeGrace)
				if now.Before(a.FixDue) {
					waiting = append(waiting, a)
					continue
				}
			}
		}
		blocking = append(blocking, a)
	}
	return blocking, waiting
}

// vulnVerdict turns a lane's sorted blocking lines and its pending fixes into the shared
// verdict: any blocking line fails (`remedy` says what to do about it, "" for nothing, and the
// waiting ones ride along), a waiting one alone warns, and nothing at all passes with okMessage.
// `extraWarnings` are lines that warn on their own account (cargo-audit's unmaintained crates).
func vulnVerdict(
	tool string, blockingLines []string, remedy string,
	pending map[vulnPendingFix]*vulnPendingGroup, extraWarnings []string, okMessage string,
) (CheckResult, error) {
	waitingLines, waitingCount := formatPendingFixes(pending)
	if len(blockingLines) > 0 {
		lines := blockingLines
		if remedy != "" {
			lines = append(lines, "", remedy)
		}
		if len(waitingLines) > 0 {
			lines = append(append(lines, "", "Also not failing yet:"), waitingLines...)
		}
		if len(extraWarnings) > 0 {
			lines = append(append(lines, ""), extraWarnings...)
		}
		return CheckResult{}, fmt.Errorf("%s found vulnerabilities\n%s", tool, indentOutput(strings.Join(lines, "\n")))
	}
	if waitingCount > 0 || len(extraWarnings) > 0 {
		var headline []string
		var lines []string
		if waitingCount > 0 {
			headline = append(headline, fmt.Sprintf("%d %s whose fix is under six days old; upgrade once it's adoptable, before it fails",
				waitingCount, Pluralize(waitingCount, "vulnerability", "vulnerabilities")))
			lines = append(lines, waitingLines...)
		}
		if len(extraWarnings) > 0 {
			headline = append(headline, fmt.Sprintf("%d %s", len(extraWarnings), Pluralize(len(extraWarnings), "warning", "warnings")))
			if len(lines) > 0 {
				lines = append(lines, "")
			}
			lines = append(lines, extraWarnings...)
		}
		msg := strings.Join(headline, "; ") + "\n" + indentOutput(strings.Join(lines, "\n"))
		return CheckResult{Code: ResultWarning, Message: msg, Total: -1, Issues: waitingCount + len(extraWarnings), Changes: -1}, nil
	}
	return Success(okMessage), nil
}
