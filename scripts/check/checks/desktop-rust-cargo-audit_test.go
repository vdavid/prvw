package checks

import (
	"strings"
	"testing"
	"time"
)

// cargoAuditFixture is trimmed from a real `cargo audit --json` run (cargo-audit 0.22.2,
// 2026-10-16) against the lockfile that still had rustls 0.23.45, plus one unmaintained warning.
const cargoAuditFixture = `{"lockfile":{"dependency-count":606},"vulnerabilities":{"found":true,"count":2,"list":[
{"advisory":{"id":"RUSTSEC-2026-0346","title":"` + "`Debug`" + ` impls for client session values expose TLS secrets","date":"2026-10-14"},"versions":{"patched":[">=0.23.46"],"unaffected":["<0.21.0"]},"package":{"name":"rustls","version":"0.23.45"}},
{"advisory":{"id":"RUSTSEC-2026-0349","title":"Unauthenticated ServerHello can downgrade later connections to classical key exchange","date":"2026-10-14"},"versions":{"patched":[">=0.23.46"],"unaffected":[]},"package":{"name":"rustls","version":"0.23.45"}}]},
"warnings":{"unmaintained":[{"advisory":{"id":"RUSTSEC-2099-0001","title":"oldcrate is unmaintained"},"package":{"name":"oldcrate","version":"1.0.0"}}]}}`

func TestCargoAuditAdvisories(t *testing.T) {
	report, err := parseAuditJSON(cargoAuditFixture)
	if err != nil {
		t.Fatal(err)
	}
	got := cargoAuditAdvisories(report)
	want := []vulnAdvisory{
		{ID: "RUSTSEC-2026-0346", Summary: "`Debug` impls for client session values expose TLS secrets",
			Ecosystem: "crates.io", Module: "rustls", Version: "0.23.45"},
		{ID: "RUSTSEC-2026-0349", Summary: "Unauthenticated ServerHello can downgrade later connections to classical key exchange",
			Ecosystem: "crates.io", Module: "rustls", Version: "0.23.45"},
	}
	if len(got) != len(want) {
		t.Fatalf("want %d advisories, got %d: %+v", len(want), len(got), got)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("advisory %d =\n  %+v\nwant\n  %+v", i, got[i], want[i])
		}
	}
}

// The rustls case that turned the daily scan red: 0.23.46 fixed four advisories on
// 2026-10-14 09:54 UTC, so it's adoptable from 2026-10-17 09:54 and fails from 2026-10-20 09:54.
func TestCargoAuditAdvisoriesFollowTheFixAgePolicy(t *testing.T) {
	released := time.Date(2026, 10, 14, 9, 54, 0, 0, time.UTC)
	report, err := parseAuditJSON(cargoAuditFixture)
	if err != nil {
		t.Fatal(err)
	}
	advisories := cargoAuditAdvisories(report)
	withFix := func(a vulnAdvisory) vulnAdvisory {
		a.FixedVersion = "0.23.46"
		return a
	}
	fixReleased := func(a vulnAdvisory) (time.Time, bool) { return released, a.FixedVersion == "0.23.46" }

	blocking, waiting := splitCargoAuditAdvisories(advisories, time.Date(2026, 10, 16, 9, 0, 0, 0, time.UTC), withFix, fixReleased)
	if len(blocking) != 0 || len(waiting) != 2 {
		t.Fatalf("two days in, both should wait, got blocking %+v, waiting %+v", blocking, waiting)
	}
	if want := time.Date(2026, 10, 17, 9, 54, 0, 0, time.UTC); !waiting[0].FixAdoptable.Equal(want) {
		t.Errorf("FixAdoptable = %s, want %s", waiting[0].FixAdoptable, want)
	}
	if want := time.Date(2026, 10, 20, 9, 54, 0, 0, time.UTC); !waiting[0].FixDue.Equal(want) {
		t.Errorf("FixDue = %s, want %s", waiting[0].FixDue, want)
	}

	blocking, waiting = splitCargoAuditAdvisories(advisories, time.Date(2026, 10, 20, 9, 54, 0, 0, time.UTC), withFix, fixReleased)
	if len(blocking) != 2 || len(waiting) != 0 {
		t.Errorf("once the grace ends both should fail, got blocking %+v, waiting %+v", blocking, waiting)
	}

	noFix := func(a vulnAdvisory) vulnAdvisory { return a }
	blocking, _ = splitCargoAuditAdvisories(advisories, time.Date(2026, 10, 16, 9, 0, 0, 0, time.UTC), noFix, fixReleased)
	if len(blocking) != 2 {
		t.Errorf("an advisory with no known fix fails at once, got %+v", blocking)
	}
}

// Waiting advisories and unmaintained warnings both warn, in one message; a blocking one fails
// and carries the rest along.
func TestVulnVerdictWithExtraWarnings(t *testing.T) {
	due := time.Date(2026, 10, 20, 9, 54, 0, 0, time.UTC)
	pending := map[vulnPendingFix]*vulnPendingGroup{}
	addPendingFix(pending, vulnAdvisory{ID: "RUSTSEC-2026-0346", Module: "rustls", Version: "0.23.45", FixedVersion: "0.23.46",
		FixAdoptable: due.Add(-vulnUpgradeGrace), FixDue: due}, "Cargo.lock")
	extra := []string{"WARN  oldcrate 1.0.0, unmaintained: oldcrate is unmaintained (RUSTSEC-2099-0001)"}

	result, err := vulnVerdict("cargo-audit", nil, "", pending, extra, "ok")
	if err != nil {
		t.Fatalf("waiting plus a warning should warn, got error %v", err)
	}
	if result.Code != ResultWarning || result.Issues != 2 ||
		!strings.Contains(result.Message, "rustls@0.23.45") || !strings.Contains(result.Message, "oldcrate") {
		t.Errorf("want one warning naming both, got %+v", result)
	}

	_, err = vulnVerdict("cargo-audit", []string{"[Cargo.lock] RUSTSEC-X"}, "", pending, extra, "ok")
	if err == nil || !strings.Contains(err.Error(), "Also not failing yet") || !strings.Contains(err.Error(), "oldcrate") {
		t.Errorf("a blocking advisory should fail and carry the rest, got %v", err)
	}
}
