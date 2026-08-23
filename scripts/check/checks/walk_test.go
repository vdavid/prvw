package checks

import (
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"testing"
)

// writeTree creates dirs and files under root. Entries ending in "/" are
// directories; everything else is an empty file.
func writeTree(t *testing.T, root string, entries ...string) {
	t.Helper()
	for _, entry := range entries {
		full := filepath.Join(root, filepath.FromSlash(entry))
		if entry[len(entry)-1] == '/' {
			if err := os.MkdirAll(full, 0755); err != nil {
				t.Fatalf("mkdir %s: %v", entry, err)
			}
			continue
		}
		if err := os.MkdirAll(filepath.Dir(full), 0755); err != nil {
			t.Fatalf("mkdir for %s: %v", entry, err)
		}
		if err := os.WriteFile(full, nil, 0644); err != nil {
			t.Fatalf("write %s: %v", entry, err)
		}
	}
}

func TestFindFilesMatchesNestedFilesInLexicalOrder(t *testing.T) {
	root := t.TempDir()
	writeTree(t, root,
		"main.go",
		"a/one.go",
		"a/b/two.go",
		"a/notes.md",
		"z/three.go",
	)

	got, err := findFiles(root, "*.go")
	if err != nil {
		t.Fatalf("findFiles: %v", err)
	}

	want := []string{"a/b/two.go", "a/one.go", "main.go", "z/three.go"}
	if !slices.Equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestFindFilesAcceptsSeveralPatternsWithoutDuplicating(t *testing.T) {
	root := t.TempDir()
	writeTree(t, root,
		"src/page.astro",
		"src/util.ts",
		"src/util.js",
		"src/style.css",
		"src/nested/deep.ts",
	)

	got, err := findFiles(filepath.Join(root, "src"), "*.ts", "*.astro", "*.js")
	if err != nil {
		t.Fatalf("findFiles: %v", err)
	}

	want := []string{"nested/deep.ts", "page.astro", "util.js", "util.ts"}
	if !slices.Equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestFindFilesIncludesHiddenEntriesLikeFind(t *testing.T) {
	root := t.TempDir()
	writeTree(t, root,
		".hidden.go",
		".config/tucked.go",
		"visible.go",
	)

	got, err := findFiles(root, "*.go")
	if err != nil {
		t.Fatalf("findFiles: %v", err)
	}

	want := []string{".config/tucked.go", ".hidden.go", "visible.go"}
	if !slices.Equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestFindFilesSkipsDirectoriesWhoseNameMatches(t *testing.T) {
	root := t.TempDir()
	writeTree(t, root,
		"generated.go/",
		"generated.go/real.go",
	)

	got, err := findFiles(root, "*.go")
	if err != nil {
		t.Fatalf("findFiles: %v", err)
	}

	want := []string{"generated.go/real.go"}
	if !slices.Equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestFindFilesSkipsSymlinks(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("creating symlinks on Windows needs developer mode or elevation")
	}

	root := t.TempDir()
	writeTree(t, root, "real.go")
	if err := os.Symlink(filepath.Join(root, "real.go"), filepath.Join(root, "link.go")); err != nil {
		t.Fatalf("symlink: %v", err)
	}

	got, err := findFiles(root, "*.go")
	if err != nil {
		t.Fatalf("findFiles: %v", err)
	}

	want := []string{"real.go"}
	if !slices.Equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestFindFilesTreatsAMissingDirectoryAsEmpty(t *testing.T) {
	got, err := findFiles(filepath.Join(t.TempDir(), "nope"), "*.go")
	if err != nil {
		t.Fatalf("findFiles: %v", err)
	}
	if len(got) != 0 {
		t.Errorf("got %v, want no files", got)
	}
}

func TestFindFilesRejectsAMalformedPattern(t *testing.T) {
	root := t.TempDir()
	writeTree(t, root, "main.go")

	if _, err := findFiles(root, "["); err == nil {
		t.Error("want an error for a malformed pattern, got none")
	}
}

func TestCountFilesCountsMatchesAndShrugsOffAMissingDirectory(t *testing.T) {
	root := t.TempDir()
	writeTree(t, root, "a.go", "b/c.go", "d.md")

	if got := countFiles(root, "*.go"); got != 2 {
		t.Errorf("got %d, want 2", got)
	}
	if got := countFiles(filepath.Join(root, "nope"), "*.go"); got != 0 {
		t.Errorf("got %d, want 0", got)
	}
}

func TestFindGoModulesReportsTheDirectoriesHoldingEachGoMod(t *testing.T) {
	root := t.TempDir()
	writeTree(t, root,
		"go.mod",
		"check/go.mod",
		"check/checks/common.go",
		"tools/gen/go.mod",
	)

	got, err := FindGoModules(root)
	if err != nil {
		t.Fatalf("FindGoModules: %v", err)
	}

	// Callers key these into a map, so the order is not part of the contract.
	slices.Sort(got)
	want := []string{".", "check", "tools/gen"}
	if !slices.Equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}
