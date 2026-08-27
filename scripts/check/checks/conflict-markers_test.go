package checks

import (
	"os"
	"path/filepath"
	"testing"
)

// The three markers git leaves behind are caught, wherever they sit in the file.
func TestConflictMarkersAreFound(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "doc.md")
	content := "intro\n" +
		"<<<<<<< HEAD\n" +
		"ours\n" +
		"||||||| merged common ancestors\n" +
		"base\n" +
		"=======\n" +
		"theirs\n" +
		">>>>>>> branch\n" +
		"outro\n"
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}

	lines, err := conflictMarkersIn(path)
	if err != nil {
		t.Fatal(err)
	}
	// Lines 2, 4, and 8. NOT line 6: `=======` is a legal Markdown setext heading underline,
	// so matching it would fail on ordinary documents.
	want := []int{2, 4, 8}
	if len(lines) != len(want) {
		t.Fatalf("got %v, want %v", lines, want)
	}
	for i := range want {
		if lines[i] != want[i] {
			t.Fatalf("got %v, want %v", lines, want)
		}
	}
}

// The exact shape that got through: a resolution staged while the markers were still in the
// file. This is the regression this check exists for.
func TestAConflictMarkerInProseIsFound(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "AGENTS.md")
	content := "**Supported platforms.** macOS is the shipping target and the only one with\n" +
		"<<<<<<< HEAD UI, the settings window, and the updater are all macOS-only.\n" +
		"Windows has a native menu bar.\n"
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}

	lines, err := conflictMarkersIn(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(lines) != 1 || lines[0] != 2 {
		t.Fatalf("a marker reflowed into a prose line was missed: got %v, want [2]", lines)
	}
}

// A clean file reports nothing, and text that merely mentions a marker away from column zero
// isn't a finding. Otherwise this check couldn't be documented without failing on its own docs.
func TestCleanFilesAndIndentedMentionsPass(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "notes.md")
	content := "Resolving a merge means deleting the `<<<<<<< HEAD` line.\n" +
		"    <<<<<<< HEAD\n" +
		"Ordinary prose.\n" +
		"=======\n" +
		"A setext heading underline above this line is not a conflict.\n"
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}

	lines, err := conflictMarkersIn(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(lines) != 0 {
		t.Fatalf("false positives at lines %v", lines)
	}
}
