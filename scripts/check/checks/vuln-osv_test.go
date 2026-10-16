package checks

import (
	"encoding/json"
	"testing"
)

// osvRustlsRecord is trimmed from `https://api.osv.dev/v1/vulns/RUSTSEC-2026-0346` (2026-10-14).
const osvRustlsRecord = `{"id":"RUSTSEC-2026-0346","published":"2026-10-14T12:00:00Z","summary":"Debug impls expose TLS secrets",
  "affected":[{"package":{"name":"rustls","ecosystem":"crates.io"},"ranges":[{"type":"SEMVER","events":[{"introduced":"0.21.0"},{"fixed":"0.23.46"}]}]}]}`

// osvFixFor finds the fix an OSV record names for the version we use.
func TestOSVFixFor(t *testing.T) {
	var v osvVulnerability
	if err := json.Unmarshal([]byte(osvRustlsRecord), &v); err != nil {
		t.Fatal(err)
	}

	a := vulnAdvisory{ID: "RUSTSEC-2026-0346", Summary: "kept", Ecosystem: "crates.io", Module: "rustls", Version: "0.23.45"}
	got := osvFixFor(a, v)
	if got.FixedVersion != "0.23.46" || got.Summary != "kept" || !got.Published.Equal(v.Published) {
		t.Errorf("osvFixFor = %+v, want fix 0.23.46, the original summary, and the OSV publish date", got)
	}

	other := a
	other.Module = "rustls-pki-types"
	if got := osvFixFor(other, v); got.FixedVersion != "" {
		t.Errorf("a record about another crate must name no fix, got %q", got.FixedVersion)
	}
}

func TestOSVFixedVersion(t *testing.T) {
	ranges := []osvRange{
		{Type: "SEMVER", Events: []osvEvent{{Introduced: "0"}, {Fixed: "0.9.5"}, {Introduced: "0.10.0"}, {Fixed: "0.10.3"}}},
	}
	cases := []struct {
		version, want string
	}{
		{"0.9.1", "0.9.5"},
		{"0.10.2", "0.10.3"},
		{"0.10.3", ""}, // at the fix: not in this range
		{"0.9.7", ""},  // between ranges: unaffected, so no fix to name
	}
	for _, tc := range cases {
		if got := osvFixedVersion(ranges, tc.version); got != tc.want {
			t.Errorf("osvFixedVersion(%s) = %q, want %q", tc.version, got, tc.want)
		}
	}

	lastAffectedOnly := []osvRange{{Type: "ECOSYSTEM", Events: []osvEvent{{Introduced: "0"}, {LastAffected: "2.1.0"}}}}
	if got := osvFixedVersion(lastAffectedOnly, "2.0.0"); got != "" {
		t.Errorf("a range with no fix event must yield no fix, got %q", got)
	}
	gitRange := []osvRange{{Type: "GIT", Events: []osvEvent{{Introduced: "0"}, {Fixed: "abc123"}}}}
	if got := osvFixedVersion(gitRange, "1.0.0"); got != "" {
		t.Errorf("a GIT range names commits, not versions, got %q", got)
	}
}

func TestCompareLooseVersions(t *testing.T) {
	cases := []struct {
		a, b string
		want int
	}{
		{"0.11.16", "0.11.18", -1},
		{"1.10.0", "1.9.0", 1},
		{"v1.2.3", "1.2.3", 0},
		{"1.2", "1.2.0", 0},
		{"1.0.0-rc.1", "1.0.0", -1},
		{"1.0.0+build.5", "1.0.0", 0},
		{"0", "0.7.0", -1},
	}
	for _, tc := range cases {
		if got := compareLooseVersions(tc.a, tc.b); got != tc.want {
			t.Errorf("compareLooseVersions(%s, %s) = %d, want %d", tc.a, tc.b, got, tc.want)
		}
	}
}
