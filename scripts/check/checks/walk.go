package checks

import (
	"errors"
	"fmt"
	"io/fs"
	"path"
	"path/filepath"
)

// findFiles walks dir and returns every regular file whose base name matches one
// of the glob patterns. Paths come back relative to dir and slash-separated, so
// they read the same on every platform.
//
// The semantics are those of `find <dir> -type f \( -name p1 -o -name p2 \)`:
// symlinks are not regular files and never match, hidden entries count, and no
// directory is pruned. Pruning would be dead weight here, because every call
// site walks a tree that holds source only (`apps/desktop/src`,
// `apps/website/src`, `scripts`), with no `target`, `node_modules`, or `.git`
// anywhere inside it.
//
// A dir that does not exist yields no files and no error. Callers walk to count
// files for a message, and a directory that isn't there has none.
func findFiles(dir string, patterns ...string) ([]string, error) {
	var files []string

	err := filepath.WalkDir(dir, func(entryPath string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			if entryPath == dir && errors.Is(walkErr, fs.ErrNotExist) {
				return nil
			}
			return walkErr
		}
		if !entry.Type().IsRegular() {
			return nil
		}

		for _, pattern := range patterns {
			matched, matchErr := path.Match(pattern, entry.Name())
			if matchErr != nil {
				return fmt.Errorf("bad file pattern %q: %w", pattern, matchErr)
			}
			if matched {
				rel, relErr := filepath.Rel(dir, entryPath)
				if relErr != nil {
					return relErr
				}
				files = append(files, filepath.ToSlash(rel))
				return nil
			}
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return files, nil
}

// countFiles returns how many regular files under dir match the patterns. It is
// the shape most callers want: a number for a result message, and a walk that
// fails silently rather than failing a check over a file count.
func countFiles(dir string, patterns ...string) int {
	files, err := findFiles(dir, patterns...)
	if err != nil {
		return 0
	}
	return len(files)
}
