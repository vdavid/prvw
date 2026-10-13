package checks

import "testing"

// A trimmed `cargo deny --format json check` stream: cargo's progress line, a duplicate that isn't
// in `[bans] skip` (an error, since `multiple-versions = "deny"`), two warnings, and the summary.
const cargoDenySampleStream = `    Updating crates.io index
{"fields":{"code":"duplicate","labels":[],"message":"found 2 duplicate entries for crate 'pollster'","severity":"error"},"type":"diagnostic"}
{"fields":{"code":"license-exception-not-encountered","labels":[],"message":"license exception was not encountered","severity":"warning"},"type":"diagnostic"}
{"fields":{"code":"accepted","labels":[],"message":"license requirements satisfied","severity":"warning"},"type":"diagnostic"}
{"fields":{"bans":{"errors":1,"helps":0,"notes":0,"warnings":0},"licenses":{"errors":0,"helps":3,"notes":0,"warnings":2},"sources":{"errors":0,"helps":0,"notes":0,"warnings":0}},"type":"summary"}
`

func TestParseCargoDenyOutputCountsErrorsAndSortsWarnings(t *testing.T) {
	findings, err := parseCargoDenyOutput(cargoDenySampleStream)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if findings.Errors != 1 {
		t.Errorf("want the duplicate counted as an error, got %d errors", findings.Errors)
	}
	want := []string{
		"accepted: license requirements satisfied",
		"license-exception-not-encountered: license exception was not encountered",
	}
	if len(findings.Warnings) != len(want) || findings.Warnings[0] != want[0] || findings.Warnings[1] != want[1] {
		t.Errorf("want sorted warnings %v, got %v", want, findings.Warnings)
	}
}

func TestParseCargoDenyOutputRejectsAStreamWithNoSummary(t *testing.T) {
	if _, err := parseCargoDenyOutput("error: failed to load config\n"); err == nil {
		t.Fatal("want an error when cargo-deny never reached its summary")
	}
}
