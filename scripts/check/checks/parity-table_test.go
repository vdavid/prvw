package checks

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// A stand-in for the generator's output: the shape the real table has, small enough to read.
// The entry lines are 11 and 12, which is what the diff assertions below point at.
const sampleParityTable = `# Platform parity

## Summary

- macOS: 2 of 2 done, 0 not applicable, 0 missing

## Every entry

### Settings

- ` + "`AutoUpdate`" + ` "Auto-update" (setting, General, toggle, field ` + "`auto_update`" + `): macOS done, Windows missing
- ` + "`TitleBar`" + ` "Title bar" (setting, General, toggle, field ` + "`title_bar`" + `): macOS done, Windows not applicable
`

func TestParityTableFailsOnAStaleFile(t *testing.T) {
	path := writeParityFixture(t, strings.Replace(sampleParityTable, "Windows missing", "Windows done", 1))

	result, err := compareParityTable(&CheckContext{CI: true}, path, sampleParityTable)
	if err == nil {
		t.Fatalf("a stale docs/parity.md passed the check: %+v", result)
	}
	message := err.Error()
	for _, want := range []string{"parity.md", "line 11", "--check parity"} {
		if !strings.Contains(message, want) {
			t.Errorf("the failure should mention %q, got:\n%s", want, message)
		}
	}
	// CI never rewrites; it reports. Otherwise a CI run would "fix" the file and pass.
	if content, _ := os.ReadFile(path); strings.Contains(string(content), "Windows missing") {
		t.Error("the check rewrote the file in CI mode")
	}
}

func TestParityTableFailsWhenTheFileIsAbsentInCI(t *testing.T) {
	path := filepath.Join(t.TempDir(), "parity.md")

	if _, err := compareParityTable(&CheckContext{CI: true}, path, sampleParityTable); err == nil {
		t.Fatal("a missing docs/parity.md passed the check")
	}
}

func TestParityTablePassesOnACurrentFile(t *testing.T) {
	path := writeParityFixture(t, sampleParityTable)

	result, err := compareParityTable(&CheckContext{CI: true}, path, sampleParityTable)
	if err != nil {
		t.Fatalf("a current docs/parity.md failed the check: %v", err)
	}
	if result.MadeChanges {
		t.Error("nothing needed changing, but the check reported changes")
	}
	if result.Total != 2 {
		t.Errorf("expected the two entries to be counted, got %d", result.Total)
	}
}

func TestParityTableRewritesAStaleFileLocally(t *testing.T) {
	path := writeParityFixture(t, "# Platform parity\n\nstale\n")

	result, err := compareParityTable(&CheckContext{CI: false}, path, sampleParityTable)
	if err != nil {
		t.Fatalf("the local run should fix the file, not fail: %v", err)
	}
	if !result.MadeChanges {
		t.Error("the file was rewritten, but the check didn't report changes")
	}
	content, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading the rewritten file: %v", err)
	}
	if string(content) != sampleParityTable {
		t.Errorf("the file wasn't regenerated, it holds:\n%s", content)
	}
}

func TestParityTableRejectsAnEmptyGeneration(t *testing.T) {
	path := writeParityFixture(t, "# Platform parity\n")

	if _, err := compareParityTable(&CheckContext{CI: false}, path, "# Platform parity\n"); err == nil {
		t.Fatal("a table with no entries passed, so a broken generator would look green")
	}
}

func TestParityTableDiffNamesTheFirstDifference(t *testing.T) {
	diff := parityTableDiff("same\nold line\ntail\n", "same\nnew line\ntail\n")

	if !strings.Contains(diff, "line 2") {
		t.Errorf("the diff should point at line 2, got:\n%s", diff)
	}
	if !strings.Contains(diff, "old line") || !strings.Contains(diff, "new line") {
		t.Errorf("the diff should show both sides, got:\n%s", diff)
	}
	if diff := parityTableDiff("a\nb\n", "a\nb\n"); diff != "" {
		t.Errorf("identical content reported a difference: %s", diff)
	}
}

func TestParityTableDiffReportsALengthMismatch(t *testing.T) {
	diff := parityTableDiff("a\n", "a\nb\n")

	if !strings.Contains(diff, "line 2") || !strings.Contains(diff, "b") {
		t.Errorf("a shorter committed file should be reported, got:\n%s", diff)
	}
}

// writeParityFixture puts `content` in a temp file named like the real one, so failure
// messages read the way they will in a real run.
func writeParityFixture(t *testing.T, content string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "parity.md")
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatalf("writing the fixture: %v", err)
	}
	return path
}
