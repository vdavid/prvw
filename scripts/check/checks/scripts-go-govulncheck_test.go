package checks

import (
	"strings"
	"testing"
	"time"
)

// A trimmed `govulncheck -format json` stream: one advisory reached from our code (a frame
// with a function), one only imported (package-level frames, which text mode doesn't fail on
// either), and the same advisory reported through two call sites.
const govulnSampleStream = `{"config":{"protocol_version":"v1.0.0"}}
{"osv":{"id":"GO-2026-6603","summary":"Unbounded memory in net/http","published":"2026-10-08T22:31:09Z"}}
{"osv":{"id":"GO-2026-6604","summary":"Race in os","published":"2026-10-08T22:31:09Z"}}
{"osv":{"id":"GO-2026-5024","summary":"Panic in x/net","published":"2026-05-22T18:28:47Z"}}
{"finding":{"osv":"GO-2026-6603","fixed_version":"v1.27.2","trace":[{"module":"stdlib","version":"v1.27.1","package":"net/http/internal/http2","function":"RoundTrip"}]}}
{"finding":{"osv":"GO-2026-6603","fixed_version":"v1.27.2","trace":[{"module":"stdlib","version":"v1.27.1","package":"net/http/internal/http2","function":"Close"}]}}
{"finding":{"osv":"GO-2026-6604","fixed_version":"v1.27.2","trace":[{"module":"stdlib","version":"v1.27.1","package":"os"}]}}
{"finding":{"osv":"GO-2026-5024","fixed_version":"v0.44.0","trace":[{"module":"golang.org/x/net","version":"v0.43.0","package":"golang.org/x/net/html","function":"Parse"}]}}
`

func TestParseGovulnStreamKeepsOnlyCalledAdvisoriesOnce(t *testing.T) {
	vulns, err := parseGovulnStream(govulnSampleStream)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if len(vulns) != 2 {
		t.Fatalf("want 2 called advisories, got %d: %+v", len(vulns), vulns)
	}
	got := vulns[0]
	if got.ID != "GO-2026-6603" || got.Module != "stdlib" || got.Version != "v1.27.1" ||
		got.FixedVersion != "v1.27.2" || got.Summary != "Unbounded memory in net/http" {
		t.Errorf("unexpected first advisory: %+v", got)
	}
	if !got.Published.Equal(time.Date(2026, 10, 8, 22, 31, 9, 0, time.UTC)) {
		t.Errorf("published not carried over: %v", got.Published)
	}
	if vulns[1].ID != "GO-2026-5024" || vulns[1].Module != "golang.org/x/net" {
		t.Errorf("unexpected second advisory: %+v", vulns[1])
	}
}

func TestParseGovulnStreamRejectsNonJSON(t *testing.T) {
	if _, err := parseGovulnStream("go: cannot find main module\n"); err == nil {
		t.Fatal("want an error for output govulncheck didn't write as JSON")
	}
}

func TestSplitByFixAgeHoldsBackOnlyFixesInsideTheWindow(t *testing.T) {
	now := time.Date(2026, 10, 10, 8, 0, 0, 0, time.UTC)
	fresh := vulnAdvisory{ID: "GO-fresh", Module: "stdlib", FixedVersion: "v1.27.2"}
	old := vulnAdvisory{ID: "GO-old", Module: "golang.org/x/net", FixedVersion: "v0.44.0"}
	unfixed := vulnAdvisory{ID: "GO-unfixed", Module: "example.com/m"}
	unknownDate := vulnAdvisory{ID: "GO-unknown", Module: "example.com/n", FixedVersion: "v2.0.0"}
	adoptable := vulnAdvisory{ID: "GO-adoptable", Module: "golang.org/x/text", FixedVersion: "v0.30.0"}
	released := map[string]time.Time{
		"GO-fresh":     now.Add(-40 * time.Hour),
		"GO-adoptable": now.Add(-100 * time.Hour),
		"GO-old":       now.Add(-90 * 24 * time.Hour),
	}
	fixReleased := func(a vulnAdvisory) (time.Time, bool) {
		at, ok := released[a.ID]
		return at, ok
	}

	blocking, waiting := splitByFixAge([]vulnAdvisory{fresh, adoptable, old, unfixed, unknownDate}, now, fixReleased)

	if len(waiting) != 2 || waiting[0].ID != "GO-fresh" || waiting[1].ID != "GO-adoptable" {
		t.Fatalf("want GO-fresh and the adoptable-but-not-yet-due GO-adoptable held back, got %+v", waiting)
	}
	if !waiting[0].FixAdoptable.Equal(now.Add(32 * time.Hour)) {
		t.Errorf("want the fix adoptable three days after release, got %v", waiting[0].FixAdoptable)
	}
	if !waiting[0].FixDue.Equal(now.Add(104 * time.Hour)) {
		t.Errorf("want the fix due six days after release, got %v", waiting[0].FixDue)
	}
	var ids []string
	for _, b := range blocking {
		ids = append(ids, b.ID)
	}
	if strings.Join(ids, ",") != "GO-old,GO-unfixed,GO-unknown" {
		t.Errorf("want old, unfixed, and undatable fixes to block, got %v", ids)
	}
}

func TestSplitByFixAgeBlocksOnceTheGraceRunsOut(t *testing.T) {
	now := time.Date(2026, 10, 14, 22, 31, 9, 0, time.UTC)
	a := vulnAdvisory{ID: "GO-edge", Module: "stdlib", FixedVersion: "v1.27.2"}
	split := func(age time.Duration) (blocking, waiting []vulnAdvisory) {
		return splitByFixAge([]vulnAdvisory{a}, now, func(vulnAdvisory) (time.Time, bool) { return now.Add(-age), true })
	}
	if blocking, waiting := split(vulnFixAdoptionWindow); len(blocking) != 0 || len(waiting) != 1 {
		t.Errorf("a fix exactly three days old is adoptable but not yet due, so it warns: blocking=%v waiting=%v", blocking, waiting)
	}
	if blocking, waiting := split(vulnFixAdoptionWindow + vulnUpgradeGrace); len(blocking) != 1 || len(waiting) != 0 {
		t.Errorf("a fix exactly six days old is due, so it blocks: blocking=%v waiting=%v", blocking, waiting)
	}
}

func TestStdlibFixIsDatedByItsAdvisory(t *testing.T) {
	published := time.Date(2026, 10, 8, 22, 31, 9, 0, time.UTC)
	a := vulnAdvisory{ID: "GO-1", Module: "stdlib", FixedVersion: "v1.27.2", Published: published}
	at, ok := govulnFixReleased(a, func(string, string) (time.Time, bool) {
		t.Fatal("stdlib must not ask the module proxy")
		return time.Time{}, false
	})
	if !ok || !at.Equal(published) {
		t.Errorf("want the advisory's publish time, got %v %v", at, ok)
	}
}

func TestModuleFixIsDatedByTheProxy(t *testing.T) {
	proxyTime := time.Date(2026, 9, 1, 0, 0, 0, 0, time.UTC)
	a := vulnAdvisory{ID: "GO-2", Module: "golang.org/x/net", FixedVersion: "v0.44.0",
		Published: time.Date(2026, 10, 9, 0, 0, 0, 0, time.UTC)}
	at, ok := govulnFixReleased(a, func(module, version string) (time.Time, bool) {
		if module != "golang.org/x/net" || version != "v0.44.0" {
			t.Errorf("asked the proxy about %s@%s", module, version)
		}
		return proxyTime, true
	})
	if !ok || !at.Equal(proxyTime) {
		t.Errorf("want the proxy's release time, got %v %v", at, ok)
	}
}

func TestPendingFixesCollapseToOneLinePerFix(t *testing.T) {
	adoptable := time.Date(2026, 10, 11, 22, 31, 9, 0, time.UTC)
	stdlib := func(id string) vulnAdvisory {
		return vulnAdvisory{ID: id, Module: "stdlib", Version: "v1.27.1", FixedVersion: "v1.27.2", FixAdoptable: adoptable, FixDue: adoptable.Add(vulnUpgradeGrace)}
	}
	groups := map[vulnPendingFix]*vulnPendingGroup{}
	addPendingFix(groups, stdlib("GO-2026-6611"), "scripts/check")
	addPendingFix(groups, stdlib("GO-2026-6603"), "scripts/check")
	addPendingFix(groups, stdlib("GO-2026-6603"), "xtask/scripts")

	lines, count := formatPendingFixes(groups)

	if count != 2 {
		t.Errorf("want two distinct advisories, got %d", count)
	}
	want := "stdlib@v1.27.1, fixed in v1.27.2, adoptable 2026-10-11 22:31 UTC, fails 2026-10-14 22:31 UTC: GO-2026-6603, GO-2026-6611 (in scripts/check, xtask/scripts)"
	if len(lines) != 1 || lines[0] != want {
		t.Errorf("got %q", lines)
	}
}

func TestEscapeModulePathForProxy(t *testing.T) {
	if got := escapeModulePath("github.com/BurntSushi/toml"); got != "github.com/!burnt!sushi/toml" {
		t.Errorf("got %q", got)
	}
}
