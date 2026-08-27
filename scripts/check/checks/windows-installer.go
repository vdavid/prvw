package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
)

// installerRegistryPath is the generated NSIS include, relative to the repo root.
var installerRegistryPath = filepath.Join("apps", "desktop", "installer", "windows", "file-associations.nsh")

// installerScriptPath is the hand-written installer, relative to the repo root.
var installerScriptPath = filepath.Join("apps", "desktop", "installer", "windows", "prvw.nsi")

// installerWriteLine matches one generated registry write, which is how the check knows the
// generator produced a registration rather than an empty pair of macros.
var installerWriteLine = regexp.MustCompile(`(?m)^  WriteRegStr HKCU `)

// crateVersionLine reads the desktop crate's version out of its manifest. The installer must not
// carry a second copy of it: the build script passes `-DPRVW_VERSION` from this same line.
var crateVersionLine = regexp.MustCompile(`(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"`)

// utf8BOM is what keeps makensis from reading the .nsi in the build host's ANSI code page, which
// is the difference between "Rymdskottkärra AB" and whatever the locale makes of those bytes.
const utf8BOM = "\xEF\xBB\xBF"

// RunWindowsInstaller keeps the Windows installer's sources honest. Three things, all of which a
// Mac can answer:
//
//  1. `file-associations.nsh` still matches `settings::windows::file_types`. The installer and
//     the app's own "Register Prvw's file types" button write the same keys, and this is what
//     makes that a fact rather than a claim.
//  2. `prvw.nsi` is still UTF-8 with a BOM.
//  3. `prvw.nsi` carries no version number of its own, so it can't drift from the crate's.
//
// It doesn't run makensis: that packages a 36 MB executable, and there's no syntax-only mode.
// `./scripts/build-windows-installer.sh` is what proves the script compiles.
func RunWindowsInstaller(ctx *CheckContext) (CheckResult, error) {
	if _, err := os.Stat(filepath.Join(ctx.RootDir, "xtask", "Cargo.toml")); os.IsNotExist(err) {
		return Skipped("xtask/Cargo.toml not found"), nil
	}
	if _, err := os.Stat(filepath.Join(ctx.RootDir, installerScriptPath)); os.IsNotExist(err) {
		return Skipped("apps/desktop/installer/windows not found"), nil
	}

	if err := checkInstallerScript(ctx.RootDir); err != nil {
		return CheckResult{}, err
	}

	generated, err := generateInstallerRegistry(ctx.RootDir)
	if err != nil {
		return CheckResult{}, err
	}
	return compareInstallerRegistry(ctx, filepath.Join(ctx.RootDir, installerRegistryPath), generated)
}

// checkInstallerScript covers the two things about `prvw.nsi` that fail silently: a lost BOM
// mangles the publisher name in the installer's version info, and a hardcoded version drifts
// from the crate's the first time someone releases without thinking about it.
func checkInstallerScript(rootDir string) error {
	script, err := os.ReadFile(filepath.Join(rootDir, installerScriptPath))
	if err != nil {
		return fmt.Errorf("failed to read %s: %w", installerScriptPath, err)
	}
	if !strings.HasPrefix(string(script), utf8BOM) {
		return fmt.Errorf("%s lost its UTF-8 BOM, so makensis will read it in the build host's code page and mangle the publisher name", installerScriptPath)
	}
	if !strings.Contains(string(script), "${PRVW_VERSION}") {
		return fmt.Errorf("%s never uses ${PRVW_VERSION}, so the installer wouldn't carry a version at all", installerScriptPath)
	}

	manifest, err := os.ReadFile(filepath.Join(rootDir, "apps", "desktop", "Cargo.toml"))
	if err != nil {
		return fmt.Errorf("failed to read the desktop crate's manifest: %w", err)
	}
	match := crateVersionLine.FindSubmatch(manifest)
	if match == nil {
		return fmt.Errorf("couldn't read a version out of apps/desktop/Cargo.toml")
	}
	if version := string(match[1]); strings.Contains(string(script), version) {
		return fmt.Errorf("%s spells out %s. The version comes from apps/desktop/Cargo.toml through -DPRVW_VERSION; a second copy drifts", installerScriptPath, version)
	}
	return nil
}

// generateInstallerRegistry runs the xtask and returns its stdout, which is the whole include.
func generateInstallerRegistry(rootDir string) (string, error) {
	cmd := exec.Command("cargo", "run", "--quiet", "--package", "xtask", "--", "installer-registry")
	cmd.Dir = rootDir
	// The same separate target directory the parity generator uses, so neither waits behind the
	// app's build lock. xtask has no dependencies, so it stays tiny.
	cmd.Env = append(os.Environ(), "CARGO_TARGET_DIR="+filepath.Join(rootDir, "target", "xtask"))

	stdout, stderr, err := RunCommandSplit(cmd)
	if err != nil {
		return "", fmt.Errorf("generating the installer's file-type registration failed\n%s", indentOutput(stderr))
	}
	return stdout, nil
}

// compareInstallerRegistry is the half that decides: CI fails on a difference, and a local run
// rewrites the file the way the formatters do.
func compareInstallerRegistry(ctx *CheckContext, path string, generated string) (CheckResult, error) {
	writes := len(installerWriteLine.FindAllString(generated, -1))
	if writes == 0 {
		return CheckResult{}, fmt.Errorf("the generator produced no registry writes, so the installer would register nothing")
	}

	committed, readErr := os.ReadFile(path)
	if readErr != nil && !os.IsNotExist(readErr) {
		return CheckResult{}, fmt.Errorf("failed to read %s: %w", path, readErr)
	}
	if readErr == nil && string(committed) == generated {
		result := Success(fmt.Sprintf("%d registry %s, %s is current", writes, Pluralize(writes, "write", "writes"), filepath.Base(path)))
		result.Total = writes
		result.Issues = 0
		result.Changes = 0
		return result, nil
	}

	if ctx.CI {
		if os.IsNotExist(readErr) {
			return CheckResult{}, fmt.Errorf("%s is missing, generate it with `./scripts/check.sh --check installer` and commit it", filepath.Base(path))
		}
		return CheckResult{}, fmt.Errorf(
			"%s no longer matches `settings::windows::file_types`. Run `./scripts/check.sh --check installer` locally and commit the result.\n%s",
			filepath.Base(path), generatedFileDiff(string(committed), generated))
	}

	if err := os.WriteFile(path, []byte(generated), 0644); err != nil {
		return CheckResult{}, fmt.Errorf("failed to write %s: %w", path, err)
	}
	result := SuccessWithChanges(fmt.Sprintf("Regenerated %s (%d registry %s)", filepath.Base(path), writes, Pluralize(writes, "write", "writes")))
	result.Total = writes
	result.Issues = 1
	result.Changes = 1
	return result, nil
}
