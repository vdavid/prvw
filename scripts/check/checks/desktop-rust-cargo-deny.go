package checks

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
)

// cargoDenyVersion is the exact cargo-deny release the check installs. Renovate bumps it through
// the custom manager in `renovate.json`, and the config format in `deny.toml` is read by this
// version, so a bump that changes the schema shows up here first.
const cargoDenyVersion = "0.20.2"

// CargoToolsRootPath is the `cargo install --root` for the pinned cargo tools. Repo-local, under
// the gitignored `target/`, for the same reason as `GoToolsBinPath`: a global `~/.cargo/bin` copy
// is whatever version the developer last installed for some other project.
const CargoToolsRootPath = "target/check-cargo-tools"

// The three cargo-deny checks that depend only on `Cargo.lock` and `deny.toml`. Advisories are
// left to cargo-audit (see the top of `deny.toml`), which is why they aren't named here.
var cargoDenyChecks = []string{"bans", "licenses", "sources"}

// cargoDenyLine is one line of `cargo deny --format json` output, which writes a diagnostic per
// finding and then a per-check summary.
type cargoDenyLine struct {
	Type   string `json:"type"`
	Fields struct {
		Code     string `json:"code"`
		Message  string `json:"message"`
		Severity string `json:"severity"`
	} `json:"fields"`
}

// cargoDenyFindings is what a passing run still has to say: crates resolved in more than one
// version, and any other warning (an unused exception, a license that only parsed leniently).
type cargoDenyFindings struct {
	Duplicates []string
	Warnings   []string
	Errors     int
}

// RunCargoDeny checks third-party crates' licenses, sources, and version requirements against
// `deny.toml`, across every target platform it lists.
func RunCargoDeny(ctx *CheckContext) (CheckResult, error) {
	if _, err := os.Stat(filepath.Join(ctx.RootDir, "deny.toml")); err != nil {
		return Skipped("no deny.toml"), nil
	}

	bin, err := ensureCargoDeny(ctx.RootDir)
	if err != nil {
		return CheckResult{}, err
	}

	args := append([]string{"--format", "json", "--color", "never", "check", "--hide-inclusion-graph"}, cargoDenyChecks...)
	cmd := exec.Command(bin, args...)
	cmd.Dir = ctx.RootDir
	stdout, stderr, runErr := RunCommandSplit(cmd)

	findings, parseErr := parseCargoDenyOutput(stderr)
	if runErr != nil || parseErr != nil || findings.Errors > 0 {
		// The JSON is for counting; the human report is what says which crate, which rule, and what
		// pulls it in, so a failure reruns in that form. Errors only: the 30-odd duplicate warnings
		// would bury them. The other warnings follow, since an unmatched exception is often the
		// reason a license failed.
		human := exec.Command(bin, append([]string{"--log-level", "error", "--color", "never", "check"}, cargoDenyChecks...)...)
		human.Dir = ctx.RootDir
		output, _ := RunCommand(human, true)
		if strings.TrimSpace(output) == "" {
			output = stdout + stderr
		}
		if len(findings.Warnings) > 0 {
			output += "\n" + strings.Join(findings.Warnings, "\n")
		}
		return CheckResult{}, fmt.Errorf("cargo-deny found problems\n%s", indentOutput(output))
	}

	summary := formatCargoDenyFindings(findings)
	if summary == "" {
		return Success(strings.Join(cargoDenyChecks, ", ") + " ok"), nil
	}
	return CheckResult{
		Code:    ResultWarning,
		Message: summary,
		Total:   -1, Issues: -1, Changes: -1,
	}, nil
}

// ensureCargoDeny returns the path to the pinned cargo-deny, installing it when the binary is
// missing or reports a different version. Like `EnsureGoTool`, it never trusts a `cargo-deny`
// found on PATH.
func ensureCargoDeny(rootDir string) (string, error) {
	root := filepath.Join(rootDir, CargoToolsRootPath)
	bin := filepath.Join(root, "bin", "cargo-deny")
	if output, err := RunCommand(exec.Command(bin, "--version"), true); err == nil &&
		strings.TrimSpace(output) == "cargo-deny "+cargoDenyVersion {
		return bin, nil
	}

	installCmd := exec.Command("cargo", "install", "cargo-deny", "--version", cargoDenyVersion, "--locked", "--root", root)
	if output, err := RunCommand(installCmd, true); err != nil {
		return "", fmt.Errorf("failed to install cargo-deny %s: %w\n%s", cargoDenyVersion, err, indentOutput(output))
	}
	return bin, nil
}

// parseCargoDenyOutput reads the JSON-lines stream cargo-deny writes to stderr. Lines that aren't
// JSON (cargo's own progress, a log line) are skipped; a stream with no summary at all is an
// error, since it means cargo-deny never got as far as checking.
func parseCargoDenyOutput(output string) (cargoDenyFindings, error) {
	var findings cargoDenyFindings
	sawSummary := false
	for raw := range strings.SplitSeq(output, "\n") {
		raw = strings.TrimSpace(raw)
		if !strings.HasPrefix(raw, "{") {
			continue
		}
		var line cargoDenyLine
		if err := json.Unmarshal([]byte(raw), &line); err != nil {
			continue
		}
		switch line.Type {
		case "summary":
			sawSummary = true
		case "diagnostic":
			switch line.Fields.Severity {
			case "error":
				findings.Errors++
			case "warning":
				if line.Fields.Code == "duplicate" {
					findings.Duplicates = append(findings.Duplicates, duplicateCrateName(line.Fields.Message))
				} else {
					findings.Warnings = append(findings.Warnings, fmt.Sprintf("%s: %s", line.Fields.Code, line.Fields.Message))
				}
			}
		}
	}
	if !sawSummary {
		return findings, fmt.Errorf("no summary in cargo-deny output")
	}
	sort.Strings(findings.Duplicates)
	return findings, nil
}

// duplicateCrateName pulls `bitflags` out of "found 2 duplicate entries for crate 'bitflags'",
// falling back to the whole message if the wording ever changes.
func duplicateCrateName(message string) string {
	start := strings.Index(message, "'")
	end := strings.LastIndex(message, "'")
	if start < 0 || end <= start {
		return message
	}
	return message[start+1 : end]
}

// formatCargoDenyFindings builds the warning text, or "" when there's nothing to say.
func formatCargoDenyFindings(findings cargoDenyFindings) string {
	var lines []string
	if n := len(findings.Duplicates); n > 0 {
		lines = append(lines, fmt.Sprintf("%d %s in more than one version: %s",
			n, Pluralize(n, "crate", "crates"), strings.Join(findings.Duplicates, ", ")))
	}
	lines = append(lines, findings.Warnings...)
	return strings.Join(lines, "\n")
}
