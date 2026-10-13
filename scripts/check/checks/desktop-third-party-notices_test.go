package checks

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

// A trimmed `deny.toml` in the shape the real one keeps: comment lines inside the arrays, inline
// exception tables one per line, and later `allow-*` keys that mustn't be mistaken for the list.
const sampleDenyToml = `[graph]
# Every platform Prvw builds for.
targets = [
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
]

[licenses]
allow = [
  "MIT",
  # Apache-2.0 plus a permission, "quoted" in a comment.
  "Apache-2.0",
]
exceptions = [
  { crate = "prvw", allow = ["BUSL-1.1"] },
  { crate = "rawler", allow = ["LGPL-2.1-only"] },
  { crate = "lensfun", allow = ["LGPL-3.0-or-later"] },
]

[sources]
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
`

func TestTomlStringArraySkipsCommentsAndStopsAtTheBracket(t *testing.T) {
	if got := tomlStringArray(sampleDenyToml, "allow"); !slices.Equal(got, []string{"MIT", "Apache-2.0"}) {
		t.Errorf("allow: got %v", got)
	}
	want := []string{"aarch64-apple-darwin", "x86_64-pc-windows-msvc"}
	if got := tomlStringArray(sampleDenyToml, "targets"); !slices.Equal(got, want) {
		t.Errorf("targets: got %v, want %v", got, want)
	}
}

func TestDenyExceptionLicensesReadsEachInlineTable(t *testing.T) {
	want := []string{"BUSL-1.1", "LGPL-2.1-only", "LGPL-3.0-or-later"}
	if got := denyExceptionLicenses(sampleDenyToml); !slices.Equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestRenderAboutConfigTakesItsPolicyFromDenyToml(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "deny.toml"), []byte(sampleDenyToml), 0o644); err != nil {
		t.Fatal(err)
	}
	config, err := renderAboutConfig(root)
	if err != nil {
		t.Fatal(err)
	}
	// Allow-list order first (cargo-about's preference for an `OR`), then the exceptions, minus
	// our own BUSL.
	if !strings.Contains(config, `accepted = ["MIT", "Apache-2.0", "LGPL-2.1-only", "LGPL-3.0-or-later"]`) {
		t.Errorf("accepted list wrong:\n%s", config)
	}
	if !strings.Contains(config, `targets = ["aarch64-apple-darwin", "x86_64-pc-windows-msvc"]`) {
		t.Errorf("targets wrong:\n%s", config)
	}
}

// Every pinned file has to carry a checksum, or cargo-about rejects the config outright.
func TestEveryClarifiedFileHasAChecksum(t *testing.T) {
	for _, clarification := range licenseClarifications {
		for _, file := range clarification.files {
			if clarificationChecksums[clarification.crate+"/"+file.path] == "" {
				t.Errorf("%s/%s has no checksum", clarification.crate, file.path)
			}
		}
	}
}

func TestVerifyClarificationsCatchesAPinThatDidNotTake(t *testing.T) {
	ok := map[string][]string{
		"rawler 0.8.0":       {"LICENSE"},
		"miniz_oxide 0.8.9":  {"LICENSE"},
		"miniz_oxide 0.9.1":  {"LICENSE"},
		"lensfun 0.7.0":      {"LICENSE-LGPL-3.0", "LICENSE-GPL-3.0"},
		"some-other-crate 1": {"LICENSE-MIT", "LICENSE-APACHE"},
	}
	if err := verifyClarifications(ok); err != nil {
		t.Errorf("want the pins to hold, got %v", err)
	}
	// One version of a two-version crate falling back to scanning is enough to fail.
	ok["miniz_oxide 0.9.1"] = []string{"LICENSE", "LICENSE-MIT.md"}
	if err := verifyClarifications(ok); err == nil || !strings.Contains(err.Error(), "miniz_oxide") {
		t.Errorf("want a failure naming miniz_oxide, got %v", err)
	}
}

func TestCrateRelativeSourceDropsTheMachineSpecificPrefix(t *testing.T) {
	cases := map[string]string{
		"/Users/someone/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.0/LICENSE-MIT": "LICENSE-MIT",
		"/home/runner/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/dpi-0.1.2/sub/LICENSE":     "sub/LICENSE",
		"LICENSE": "LICENSE",
		"":        "",
	}
	for input, want := range cases {
		if got := crateRelativeSource(input); got != want {
			t.Errorf("crateRelativeSource(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestJoinNamesReadsAsEnglish(t *testing.T) {
	cases := map[string][]string{
		"`a`":                    {"a"},
		"`a` and `b`":            {"a", "b"},
		"`a`, `b`, and `c`":      {"a", "b", "c"},
		"`a`, `b`, `c`, and `d`": {"a", "b", "c", "d"},
	}
	for want, names := range cases {
		if got := joinNames(names); got != want {
			t.Errorf("joinNames(%v) = %q, want %q", names, got, want)
		}
	}
}

func TestLgplCratesNamesEachOnce(t *testing.T) {
	packages := []attributedPackage{
		{Name: "lensfun", License: "LGPL-3.0-or-later"},
		{Name: "rawler", License: "LGPL-2.1-only"},
		{Name: "rawler", License: "LGPL-2.1-only"},
		{Name: "serde", License: "MIT OR Apache-2.0"},
	}
	if got := lgplCrates(packages); !slices.Equal(got, []string{"lensfun", "rawler"}) {
		t.Errorf("got %v", got)
	}
}

// The committed list itself has to pass, against a graph that has its crate in it.
func TestTheCommittedVendoredListIsValid(t *testing.T) {
	root := filepath.Join("..", "..", "..")
	if _, err := loadVendoredCredits(root, map[string]bool{"lensfun": true}); err != nil {
		t.Fatal(err)
	}
}

func TestVendoredValidationReportsEveryProblemAtOnce(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "LICENSE"), []byte("text"), 0o644); err != nil {
		t.Fatal(err)
	}
	credits := []vendoredCredit{
		{Name: "A", Author: "x", License: "CC BY 4.0", LicenseURL: "u", URL: "u", Changes: "c", Files: []string{"gone.txt"}},
		{Name: "B", Author: "x", License: "LicenseRef-B", LicenseURL: "u", URL: "u", Changes: "c", Crate: "removed"},
		{Name: "C", Author: "x", License: "MIT", LicenseURL: "u", URL: "u", Changes: "c", LicenseFile: "LICENSE", Files: []string{"../outside"}},
		{Name: "a", Author: "x", License: "MIT", LicenseURL: "u", URL: "u", Changes: ""},
	}
	err := validateVendoredCredits(credits, root, map[string]bool{"lensfun": true})
	if err == nil {
		t.Fatal("want a failure")
	}
	for _, want := range []string{
		"`CC BY 4.0` isn't a known SPDX id",
		"`gone.txt` isn't in the repo",
		"needs a `license_file`",
		"`removed` isn't in the shipped dependency graph",
		"`../outside` isn't a clean repo-relative path",
		"another credit has the same name",
		"`changes` is empty",
		"neither `files` nor `crate` is set",
	} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("missing %q in:\n%v", want, err)
		}
	}
}

func TestParseVendoredCreditsRejectsAnUnknownField(t *testing.T) {
	raw := []byte(`{"credits": [{"name": "A", "licence_url": "typo"}]}`)
	if _, err := parseVendoredCredits(raw); err == nil {
		t.Fatal("want a failure on `licence_url`")
	}
}
