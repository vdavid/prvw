package checks

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"time"
	"unicode"
)

// RunGovulncheck scans every Go module in the repo for known vulnerabilities
// using golang.org/x/vuln/cmd/govulncheck. Static-analysis-based: only flags
// vulns reachable from your code, not every transitive dep that happens to
// have a CVE. Mirrors the role of cargo-audit on the Rust side.
//
// Prvw's one Go module is the check runner itself, and most of what it can reach is the stdlib,
// so in practice this is a check on the pinned Go version against the Go vuln DB.
//
// An advisory whose fix is younger than vulnFixAdoptionWindow + vulnUpgradeGrace is a warning,
// so the lane turns red by itself on the first run after that (`vuln-fix-age.go`).
func RunGovulncheck(ctx *CheckContext) (CheckResult, error) {
	govulnBin, err := EnsureGoTool(ctx.RootDir, "govulncheck", "golang.org/x/vuln/cmd/govulncheck@v1.8.0")
	if err != nil {
		return CheckResult{}, err
	}

	allModules, err := FindAllGoModules(ctx.RootDir)
	if err != nil {
		return CheckResult{}, fmt.Errorf("failed to find Go modules: %w", err)
	}

	now := time.Now()
	proxy := newGoProxyReleaseTimes()
	fixReleased := func(a vulnAdvisory) (time.Time, bool) { return govulnFixReleased(a, proxy.lookup) }

	var blockingLines []string
	waiting := map[vulnPendingFix]*vulnPendingGroup{}
	modCount := 0

	for goDir, modules := range allModules {
		baseDir := filepath.Join(ctx.RootDir, goDir)
		for _, mod := range modules {
			modDir := filepath.Join(baseDir, mod)
			modLabel := filepath.Join(goDir, mod)
			modCount++

			// In JSON mode govulncheck exits 0 whatever it finds, so a non-zero exit is a real
			// failure to scan.
			cmd := exec.Command(govulnBin, "-format", "json", "./...")
			cmd.Dir = modDir
			output, err := RunCommand(cmd, true)
			if err != nil {
				return CheckResult{}, fmt.Errorf("govulncheck couldn't scan %s: %w\n%s", modLabel, err, indentOutput(output))
			}
			advisories, err := parseGovulnStream(output)
			if err != nil {
				return CheckResult{}, fmt.Errorf("couldn't read govulncheck's report for %s: %w", modLabel, err)
			}

			blocking, pending := splitByFixAge(advisories, now, fixReleased)
			for _, a := range blocking {
				blockingLines = append(blockingLines, fmt.Sprintf("[%s] %s", modLabel, a.describe()))
			}
			for _, a := range pending {
				addPendingFix(waiting, a, modLabel)
			}
		}
	}
	sort.Strings(blockingLines)
	result, err := vulnVerdict("govulncheck", blockingLines, "", waiting, nil,
		fmt.Sprintf("%d %s, no vulns", modCount, Pluralize(modCount, "module", "modules")))
	result.Total = modCount
	return result, err
}

type govulnMessage struct {
	OSV *struct {
		ID        string    `json:"id"`
		Summary   string    `json:"summary"`
		Published time.Time `json:"published"`
	} `json:"osv"`
	Finding *struct {
		OSV          string `json:"osv"`
		FixedVersion string `json:"fixed_version"`
		Trace        []struct {
			Module   string `json:"module"`
			Version  string `json:"version"`
			Function string `json:"function"`
		} `json:"trace"`
	} `json:"finding"`
}

// parseGovulnStream reads `govulncheck -format json` output: a stream of JSON objects, each an
// `osv` entry or a `finding`. Only findings whose innermost frame names a FUNCTION count, the
// same reachability bar the text mode fails on; module- and package-level findings mean the
// vulnerable code is linked in but never called.
func parseGovulnStream(output string) ([]vulnAdvisory, error) {
	type osvInfo struct {
		summary   string
		published time.Time
	}
	osvs := map[string]osvInfo{}
	seen := map[string]bool{}
	var advisories []vulnAdvisory

	dec := json.NewDecoder(strings.NewReader(output))
	for {
		var msg govulnMessage
		err := dec.Decode(&msg)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return nil, fmt.Errorf("%w\n%s", err, indentOutput(output))
		}
		if msg.OSV != nil {
			osvs[msg.OSV.ID] = osvInfo{summary: msg.OSV.Summary, published: msg.OSV.Published}
		}
		f := msg.Finding
		if f == nil || len(f.Trace) == 0 || f.Trace[0].Function == "" {
			continue
		}
		frame := f.Trace[0]
		key := f.OSV + "\x00" + frame.Module
		if seen[key] {
			continue
		}
		seen[key] = true
		advisories = append(advisories, vulnAdvisory{
			ID: f.OSV, Module: frame.Module, Version: frame.Version, FixedVersion: f.FixedVersion,
		})
	}

	// The stream emits every `osv` before the findings that cite it, but filling in afterwards
	// doesn't lean on that.
	for i := range advisories {
		info := osvs[advisories[i].ID]
		advisories[i].Summary = info.summary
		advisories[i].Published = info.published
	}
	return advisories, nil
}

// govulnFixReleased dates an advisory's fix. A stdlib fix ships as a Go point release, and the
// Go team publishes the advisories with it (go1.27.2: released 2026-10-08, its GO-2026-66xx
// advisories published 2026-10-08T22:31Z), so the advisory's own date stands in. The module
// proxy's `golang.org/toolchain` timestamps can't: they predate the release by days (go1.27.2's
// reads 2026-10-02, verified 2026-10-10). A module fix is dated by the proxy, since a module's
// advisory can be published long after the version that fixed it.
func govulnFixReleased(a vulnAdvisory, proxyReleased func(module, version string) (time.Time, bool)) (time.Time, bool) {
	if a.Module == "stdlib" || a.Module == "toolchain" {
		return a.Published, !a.Published.IsZero()
	}
	return proxyReleased(a.Module, a.FixedVersion)
}

// goProxyReleaseTimes asks proxy.golang.org when a module version was published, once per
// module@version per run.
type goProxyReleaseTimes struct {
	client *http.Client
	cache  map[string]goProxyAnswer
}

type goProxyAnswer struct {
	at time.Time
	ok bool
}

func newGoProxyReleaseTimes() *goProxyReleaseTimes {
	return &goProxyReleaseTimes{client: &http.Client{Timeout: 15 * time.Second}, cache: map[string]goProxyAnswer{}}
}

func (p *goProxyReleaseTimes) lookup(module, version string) (time.Time, bool) {
	key := module + "@" + version
	if answer, hit := p.cache[key]; hit {
		return answer.at, answer.ok
	}
	answer := p.fetch(module, version)
	p.cache[key] = answer
	return answer.at, answer.ok
}

func (p *goProxyReleaseTimes) fetch(module, version string) goProxyAnswer {
	url := fmt.Sprintf("https://proxy.golang.org/%s/@v/%s.info", escapeModulePath(module), escapeModulePath(version))
	resp, err := p.client.Get(url)
	if err != nil || resp == nil {
		return goProxyAnswer{}
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return goProxyAnswer{}
	}
	var info struct {
		Time time.Time `json:"Time"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&info); err != nil || info.Time.IsZero() {
		return goProxyAnswer{}
	}
	return goProxyAnswer{at: info.Time, ok: true}
}

// escapeModulePath applies the module proxy's case encoding: each uppercase letter becomes `!`
// plus its lowercase, so `BurntSushi` is `!burnt!sushi`.
func escapeModulePath(path string) string {
	var b strings.Builder
	for _, r := range path {
		if unicode.IsUpper(r) {
			b.WriteByte('!')
			b.WriteRune(unicode.ToLower(r))
			continue
		}
		b.WriteRune(r)
	}
	return b.String()
}

// sortedKeys returns a map's keys in order, so report lines come out the same on every run.
func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
