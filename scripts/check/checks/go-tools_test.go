package checks

import "testing"

func TestPinnedModuleVersionRejectsUnpinnedPaths(t *testing.T) {
	if v, err := pinnedModuleVersion("golang.org/x/tools/cmd/deadcode@v0.51.0"); err != nil || v != "v0.51.0" {
		t.Errorf("want v0.51.0, got %q, %v", v, err)
	}
	for _, path := range []string{"golang.org/x/tools/cmd/deadcode", "golang.org/x/tools/cmd/deadcode@", "golang.org/x/tools/cmd/deadcode@latest"} {
		if _, err := pinnedModuleVersion(path); err == nil {
			t.Errorf("want %q rejected", path)
		}
	}
}

func TestParseGoToolBuildInfoReadsGoAndModuleVersions(t *testing.T) {
	output := "/repo/target/check-go-tools/deadcode: go1.27.2\n" +
		"\tpath\tgolang.org/x/tools/cmd/deadcode\n" +
		"\tmod\tgolang.org/x/tools\tv0.51.0\th1:abc=\n" +
		"\tdep\tgolang.org/x/mod\tv0.30.0\th1:def=\n"
	info, ok := parseGoToolBuildInfo(output)
	if !ok || info.GoVersion != "go1.27.2" || info.ModuleVersion != "v0.51.0" {
		t.Errorf("got %+v, %v", info, ok)
	}
	if _, ok := parseGoToolBuildInfo("not a Go binary\n"); ok {
		t.Error("want unreadable build info reported as such")
	}
}
