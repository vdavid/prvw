package checks

// The version-pinned Go analyzers the Go checks shell out to. Ported from Cmdr's
// `scripts/check/checks/tool-install.go`.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// GoToolsBinPath is where the pinned Go analyzers live. Repo-local, under the workspace's
// gitignored `target/`, on purpose: `go install` into the user's `~/go/bin` would fight whatever
// versions they keep there for other projects.
const GoToolsBinPath = "target/check-go-tools"

// goToolBuildInfo is what `go version -m <binary>` says about how a tool was built: the module
// version it came from, and the Go toolchain that compiled it. Both decide a reinstall.
type goToolBuildInfo struct {
	ModuleVersion string
	GoVersion     string
}

// EnsureGoTool returns the path to a Go tool built from the module version the caller pins,
// installing it when what's on disk doesn't match that pin or was built by a different Go than
// the one running the checks.
//
// The pin is the supply-chain control: `@latest` would run whatever was published a minute ago,
// on every developer machine and CI runner, which is the hole the three-day release window closes
// for every other dependency. Renovate keeps the pins fresh (`renovate.json`'s custom manager reads
// the `EnsureGoTool(...)` literals).
//
// ❗ It deliberately ignores whatever binary happens to be named `<name>` on PATH. Trusting PATH
// makes the pins decorative on a developer machine, and it breaks loudly rather than usefully: a
// tool built by an older Go can't read the newer stdlib's export data, which surfaces as
// "export data version 5 is greater than maximum supported version 4" (staticcheck after the Go
// 1.27.2 bump) rather than as "your tool is stale".
func EnsureGoTool(rootDir, name, installPath string) (string, error) {
	wantModuleVersion, err := pinnedModuleVersion(installPath)
	if err != nil {
		return "", err
	}
	goVersion, err := RunCommand(exec.Command("go", "env", "GOVERSION"), true)
	if err != nil {
		return "", fmt.Errorf("cannot decide whether %s is current: `go env GOVERSION` failed: %w", name, err)
	}

	binDir := filepath.Join(rootDir, GoToolsBinPath)
	binPath := filepath.Join(binDir, name)
	if goToolIsCurrent(binPath, wantModuleVersion, strings.TrimSpace(goVersion)) {
		return binPath, nil
	}

	installCmd := exec.Command("go", "install", installPath)
	installCmd.Dir = rootDir
	installCmd.Env = append(os.Environ(), "GOBIN="+binDir)
	if output, err := RunCommand(installCmd, true); err != nil {
		return "", fmt.Errorf("failed to install %s (%s): %w\n%s", name, installPath, err, output)
	}

	return binPath, nil
}

// goToolIsCurrent reports whether the binary on disk came from the pinned module version and was
// built by the wanted Go. Anything it can't read (missing file, unreadable build info) counts as
// "not current", so the caller reinstalls rather than running something unidentified.
func goToolIsCurrent(binPath, wantModuleVersion, wantGoVersion string) bool {
	if _, err := os.Stat(binPath); err != nil {
		return false
	}
	output, err := RunCommand(exec.Command("go", "version", "-m", binPath), true)
	if err != nil {
		return false
	}
	info, ok := parseGoToolBuildInfo(output)
	if !ok {
		return false
	}
	return info.ModuleVersion == wantModuleVersion && info.GoVersion == wantGoVersion
}

// parseGoToolBuildInfo reads `go version -m` output, whose first line ends in the Go toolchain
// (`/path/to/deadcode: go1.27.2`) and whose `mod` line carries the module and its version
// (`\tmod\tgolang.org/x/tools\tv0.51.0\th1:...`).
func parseGoToolBuildInfo(output string) (goToolBuildInfo, bool) {
	var info goToolBuildInfo
	for line := range strings.SplitSeq(output, "\n") {
		fields := strings.Fields(line)
		if info.GoVersion == "" && len(fields) >= 2 && strings.HasSuffix(fields[0], ":") {
			if last := fields[len(fields)-1]; strings.HasPrefix(last, "go1.") {
				info.GoVersion = last
			}
			continue
		}
		if len(fields) >= 3 && fields[0] == "mod" {
			info.ModuleVersion = fields[2]
		}
	}
	return info, info.GoVersion != "" && info.ModuleVersion != ""
}

// pinnedModuleVersion pulls the `@version` off an install path. An unpinned path (or `@latest`) is
// an error rather than a fallback: without an exact version there's nothing to compare an installed
// binary against, and a fresh checkout would pull whatever is newest.
func pinnedModuleVersion(installPath string) (string, error) {
	module, version, found := strings.Cut(installPath, "@")
	if !found || version == "" {
		return "", fmt.Errorf("tool install path %q must pin an exact version (`@vX.Y.Z`)", installPath)
	}
	if version == "latest" {
		return "", fmt.Errorf("tool install path %q must pin an exact version, never `@latest`", module)
	}
	return version, nil
}
