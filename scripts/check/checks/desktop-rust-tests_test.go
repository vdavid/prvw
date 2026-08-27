package checks

import (
	"slices"
	"testing"
)

// The Windows CI job runs check.exe directly rather than through a shell, so the fail-fast lever
// lives in the runner. This pins what it does with the env var's value.
func TestNextestArgs(t *testing.T) {
	base := []string{"nextest", "run", "--workspace"}

	if got := nextestArgs(""); !slices.Equal(got, base) {
		t.Errorf("unset should leave nextest's own fail-fast alone, got %v", got)
	}

	want := append(slices.Clone(base), "--no-fail-fast")
	for _, value := range []string{"1", "true", "yes"} {
		if got := nextestArgs(value); !slices.Equal(got, want) {
			t.Errorf("PRVW_TEST_NO_FAIL_FAST=%q should ask for the whole picture, got %v", value, got)
		}
	}
}
