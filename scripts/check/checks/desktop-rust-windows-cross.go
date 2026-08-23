package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
)

// windowsCrossTarget is the Windows triple we type-check against from a non-Windows host.
// `aarch64-pc-windows-msvc` works with the same recipe if you add the rustup target.
const windowsCrossTarget = "x86_64-pc-windows-msvc"

// RunWindowsCross type-checks and lints the desktop app for Windows from a Unix host.
//
// Marked slow in the registry, so it stays out of a plain `./scripts/check.sh` run: it needs
// `cargo-xwin` plus a one-time ~1 GB download of the MSVC CRT and Windows SDK headers.
// Setup and background: see the cross-compilation section in `AGENTS.md`.
func RunWindowsCross(ctx *CheckContext) (CheckResult, error) {
	if runtime.GOOS == "windows" {
		return Skipped("already on Windows; the clippy check covers this target"), nil
	}

	rustDir := filepath.Join(ctx.RootDir, "apps", "desktop")
	if _, err := os.Stat(filepath.Join(rustDir, "Cargo.toml")); os.IsNotExist(err) {
		return Skipped("apps/desktop/Cargo.toml not found"), nil
	}

	if !CommandExists("cargo-xwin") {
		return CheckResult{}, fmt.Errorf("cargo-xwin is missing, install it with:\n      cargo install cargo-xwin --locked")
	}
	if err := requireRustupTarget(windowsCrossTarget); err != nil {
		return CheckResult{}, err
	}
	shimDir, err := ensureLLVMLibShim(ctx.RootDir)
	if err != nil {
		return CheckResult{}, err
	}

	cmd := exec.Command("cargo", "xwin", "clippy", "--target", windowsCrossTarget, "--all-targets", "--", "-D", "warnings")
	cmd.Dir = rustDir
	cmd.Env = append(os.Environ(), "PATH="+shimDir+string(os.PathListSeparator)+os.Getenv("PATH"))
	output, err := RunCommand(cmd, true)
	if err != nil {
		return CheckResult{}, fmt.Errorf("cross-check for "+windowsCrossTarget+" failed\n%s", indentOutput(stripBuildScriptEcho(output)))
	}

	count := len(regexp.MustCompile(`(?m)^\s*Checking`).FindAllString(output, -1))
	if count == 0 {
		return Success(windowsCrossTarget + ": no warnings"), nil
	}
	result := Success(fmt.Sprintf("%s: %d %s checked, no warnings", windowsCrossTarget, count, Pluralize(count, "crate", "crates")))
	result.Total = count
	return result, nil
}

// requireRustupTarget fails with the exact install command when a rustup target is missing.
func requireRustupTarget(target string) error {
	out, err := RunCommand(exec.Command("rustup", "target", "list", "--installed"), true)
	if err != nil {
		return fmt.Errorf("couldn't list rustup targets: %w", err)
	}
	for _, line := range strings.Split(out, "\n") {
		if strings.TrimSpace(line) == target {
			return nil
		}
	}
	return fmt.Errorf("the %s rustup target is missing, add it with:\n      rustup target add %s", target, target)
}

// ensureLLVMLibShim returns a directory holding an `llvm-lib` that cc-rs can find, and creates
// it if needed.
//
// cargo-xwin ships its own clang-cl and lld-link but not the MSVC-flavored archiver, and Apple's
// command line tools don't include one. `llvm-ar` is that same archiver: it switches to lib mode
// when argv[0] contains "lib", so a symlink named `llvm-lib` is the whole fix. The rustup
// `llvm-tools` component provides the binary, which keeps this brew-free.
func ensureLLVMLibShim(rootDir string) (string, error) {
	sysroot, err := RunCommand(exec.Command("rustc", "--print", "sysroot"), true)
	if err != nil {
		return "", fmt.Errorf("couldn't find the rustc sysroot: %w", err)
	}
	host, err := rustcHostTriple()
	if err != nil {
		return "", err
	}
	llvmAr := filepath.Join(strings.TrimSpace(sysroot), "lib", "rustlib", host, "bin", "llvm-ar")
	if _, err := os.Stat(llvmAr); err != nil {
		return "", fmt.Errorf("llvm-ar is missing, add it with:\n      rustup component add llvm-tools")
	}

	shimDir := filepath.Join(rootDir, "target", "cross-check-bin")
	if err := os.MkdirAll(shimDir, 0755); err != nil {
		return "", fmt.Errorf("couldn't create %s: %w", shimDir, err)
	}
	shim := filepath.Join(shimDir, "llvm-lib")
	// Recreate every run: a toolchain update moves llvm-ar and leaves a dangling link behind.
	if existing, err := os.Readlink(shim); err == nil && existing == llvmAr {
		return shimDir, nil
	}
	_ = os.Remove(shim)
	if err := os.Symlink(llvmAr, shim); err != nil {
		return "", fmt.Errorf("couldn't link llvm-lib to %s: %w", llvmAr, err)
	}
	return shimDir, nil
}

// rustcHostTriple returns the triple rustc itself runs on, as reported by `rustc -vV`.
func rustcHostTriple() (string, error) {
	out, err := RunCommand(exec.Command("rustc", "-vV"), true)
	if err != nil {
		return "", fmt.Errorf("couldn't read the rustc version: %w", err)
	}
	for _, line := range strings.Split(out, "\n") {
		if after, ok := strings.CutPrefix(line, "host: "); ok {
			return strings.TrimSpace(after), nil
		}
	}
	return "", fmt.Errorf("`rustc -vV` printed no host triple")
}

// stripBuildScriptEcho drops the environment echo that cc-rs build scripts print on failure.
// A failing zstd-sys build emits tens of thousands of `cargo:rerun-if-env-changed` and
// `CFLAGS_… = None` lines around the one error that matters.
var buildScriptEcho = regexp.MustCompile(`^\s+(cargo:rerun-if-env-changed|[A-Z_]+ = |exit status: 0$)`)

func stripBuildScriptEcho(output string) string {
	var kept []string
	for _, line := range strings.Split(output, "\n") {
		if !buildScriptEcho.MatchString(line) {
			kept = append(kept, line)
		}
	}
	return strings.Join(kept, "\n")
}
