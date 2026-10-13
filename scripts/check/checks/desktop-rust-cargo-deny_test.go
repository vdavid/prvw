package checks

import "testing"

// A trimmed `cargo deny --format json check` stream: cargo's progress line, two duplicates, an
// unused exception, and the summary.
const cargoDenySampleStream = `    Updating crates.io index
{"fields":{"code":"duplicate","labels":[],"message":"found 2 duplicate entries for crate 'pollster'","severity":"warning"},"type":"diagnostic"}
{"fields":{"code":"duplicate","labels":[],"message":"found 2 duplicate entries for crate 'bitflags'","severity":"warning"},"type":"diagnostic"}
{"fields":{"code":"license-exception-not-encountered","labels":[],"message":"license exception was not encountered","severity":"warning"},"type":"diagnostic"}
{"fields":{"bans":{"errors":0,"helps":0,"notes":0,"warnings":2},"licenses":{"errors":0,"helps":3,"notes":0,"warnings":1},"sources":{"errors":0,"helps":0,"notes":0,"warnings":0}},"type":"summary"}
`

func TestParseCargoDenyOutputSortsDuplicatesAndKeepsOtherWarnings(t *testing.T) {
	findings, err := parseCargoDenyOutput(cargoDenySampleStream)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if findings.Errors != 0 {
		t.Errorf("want no errors, got %d", findings.Errors)
	}
	if len(findings.Duplicates) != 2 || findings.Duplicates[0] != "bitflags" || findings.Duplicates[1] != "pollster" {
		t.Errorf("want sorted duplicate names, got %v", findings.Duplicates)
	}
	if len(findings.Warnings) != 1 || findings.Warnings[0] != "license-exception-not-encountered: license exception was not encountered" {
		t.Errorf("unexpected warnings: %v", findings.Warnings)
	}
	want := "2 crates in more than one version: bitflags, pollster\nlicense-exception-not-encountered: license exception was not encountered"
	if got := formatCargoDenyFindings(findings); got != want {
		t.Errorf("format:\n got %q\nwant %q", got, want)
	}
}

func TestParseCargoDenyOutputCountsErrors(t *testing.T) {
	stream := `{"fields":{"code":"rejected","labels":[],"message":"failed to satisfy license requirements","severity":"error"},"type":"diagnostic"}
{"fields":{},"type":"summary"}
`
	findings, err := parseCargoDenyOutput(stream)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if findings.Errors != 1 {
		t.Errorf("want 1 error, got %d", findings.Errors)
	}
}

func TestParseCargoDenyOutputRejectsAStreamWithNoSummary(t *testing.T) {
	if _, err := parseCargoDenyOutput("error: failed to load config\n"); err == nil {
		t.Fatal("want an error when cargo-deny never reached its summary")
	}
}
