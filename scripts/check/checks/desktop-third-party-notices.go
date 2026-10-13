package checks

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"maps"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"sort"
	"strings"
)

// cargoAboutVersion is the exact cargo-about release the check installs, into the same
// `CargoToolsRootPath` as cargo-deny. Renovate bumps it through the custom manager in
// `renovate.json`. The binary sits behind the `cli` feature, so a plain `cargo install` would
// silently build a library and install nothing.
const cargoAboutVersion = "0.9.2"

// NoticesFileName is the generated attribution file at the repo root. `scripts/build-and-sign.sh`
// and the Windows installer ship it beside the app as `THIRD-PARTY-NOTICES.txt`.
const NoticesFileName = "THIRD-PARTY-NOTICES.md"

// noticesStampPath records what the last local run saw, so a run where nothing moved skips
// cargo-about's 20 seconds. Under the gitignored `target/`, so a fresh clone or a teardown just
// regenerates.
const noticesStampPath = "target/check-cache/third-party-notices.sha256"

// noticesInputs are the files the generated notices are a function of, besides each vendored
// credit's own files. The two Go files are here because a change to the generator is a change to
// the output.
var noticesInputs = []string{
	"Cargo.lock",
	"Cargo.toml",
	"apps/desktop/Cargo.toml",
	"deny.toml",
	vendoredCreditsPath,
	"scripts/check/checks/desktop-third-party-notices.go",
	"scripts/check/checks/third-party-vendored.go",
}

// RunThirdPartyNotices regenerates `THIRD-PARTY-NOTICES.md` from the resolved crate graph plus
// the hand-kept vendored credits (`third-party-vendored.go`), and fails in CI if the committed
// copy is stale.
//
// Prvw ships hundreds of MIT, Apache-2.0, and BSD crates compiled into one binary, two LGPL ones
// (`rawler`, `lensfun`), and a CC BY-SA lens database. Every one of those licenses wants its text
// and copyright notices to travel with the distribution, which a lockfile alone doesn't do. This
// check produces the file that does.
//
// Local runs rewrite the file, matching the formatters. `--ci` never touches the working tree
// and fails on any drift.
func RunThirdPartyNotices(ctx *CheckContext) (CheckResult, error) {
	if _, err := os.Stat(filepath.Join(ctx.RootDir, "apps", "desktop", "Cargo.toml")); err != nil {
		return Skipped("apps/desktop/Cargo.toml not found"), nil
	}
	noticesPath := filepath.Join(ctx.RootDir, NoticesFileName)
	// Missing is a legitimate first-run state, not an error.
	original, _ := os.ReadFile(noticesPath)

	stamp, err := noticesStamp(ctx.RootDir, original)
	if err != nil {
		return CheckResult{}, err
	}
	stampPath := filepath.Join(ctx.RootDir, filepath.FromSlash(noticesStampPath))
	if !ctx.CI && len(original) > 0 && readStamp(stampPath) == stamp {
		return Success(NoticesFileName + " in sync (no input changed since the last run)"), nil
	}

	generated, summary, err := generateNotices(ctx.RootDir)
	if err != nil {
		return CheckResult{}, err
	}

	changed := !bytes.Equal(generated, original)
	if ctx.CI {
		if changed {
			return CheckResult{}, fmt.Errorf(
				"%s is stale (%s). Run `./scripts/check.sh --check third-party-notices` and commit the result",
				NoticesFileName, summary)
		}
		return Success(fmt.Sprintf("%s in sync (%s)", NoticesFileName, summary)), nil
	}

	if changed {
		if err := os.WriteFile(noticesPath, generated, 0o644); err != nil {
			return CheckResult{}, fmt.Errorf("couldn't write %s: %w", NoticesFileName, err)
		}
	}
	if err := writeStamp(ctx.RootDir, stampPath, generated); err != nil {
		return CheckResult{}, err
	}
	if changed {
		return SuccessWithChanges(fmt.Sprintf("%s regenerated (%s)", NoticesFileName, summary)), nil
	}
	return Success(fmt.Sprintf("%s in sync (%s)", NoticesFileName, summary)), nil
}

// generateNotices runs cargo-about, merges the vendored credits, and renders the file, returning
// it with a one-line summary of what it covers.
func generateNotices(rootDir string) ([]byte, string, error) {
	bin, err := ensureCargoAbout(rootDir)
	if err != nil {
		return nil, "", err
	}
	rust, err := collectRustCrates(rootDir, bin)
	if err != nil {
		return nil, "", err
	}
	vendored, err := loadVendoredCredits(rootDir, rust.crateNames())
	if err != nil {
		return nil, "", err
	}
	vendoredTexts, err := readVendoredLicenseTexts(rootDir, vendored)
	if err != nil {
		return nil, "", err
	}
	summary := fmt.Sprintf("%d Rust crates, %d other credits, %d license texts",
		len(rust.packages), len(vendored), len(rust.texts)+len(vendoredTexts))
	return renderNotices(rust, vendored, vendoredTexts), summary, nil
}

// --- the input stamp ---

// noticesStamp hashes every input plus the notices file as it stands, so editing or deleting the
// committed file by hand also forces a real run.
func noticesStamp(rootDir string, notices []byte) (string, error) {
	paths := slices.Clone(noticesInputs)
	if raw, err := os.ReadFile(filepath.Join(rootDir, filepath.FromSlash(vendoredCreditsPath))); err == nil {
		// A list that doesn't parse can't be stamped, so it falls through to a real run, which
		// is what reports the parse error.
		if credits, err := parseVendoredCredits(raw); err == nil {
			for _, credit := range credits {
				paths = append(paths, credit.Files...)
				if credit.LicenseFile != "" {
					paths = append(paths, credit.LicenseFile)
				}
			}
		}
	}

	hash := sha256.New()
	fmt.Fprintf(hash, "cargo-about %s\n", cargoAboutVersion)
	for _, input := range paths {
		fmt.Fprintf(hash, "%s\n", input)
		full := filepath.Join(rootDir, filepath.FromSlash(input))
		info, err := os.Stat(full)
		switch {
		case err != nil:
			hash.Write([]byte("missing\n"))
		case info.IsDir():
			// A credited folder counts by what's in it, not by its bytes.
			entries, _ := os.ReadDir(full)
			for _, entry := range entries {
				fmt.Fprintf(hash, "%s\n", entry.Name())
			}
		default:
			contents, err := os.ReadFile(full)
			if err != nil {
				return "", fmt.Errorf("couldn't read %s: %w", input, err)
			}
			hash.Write(contents)
		}
	}
	hash.Write([]byte("notices\n"))
	hash.Write(notices)
	return hex.EncodeToString(hash.Sum(nil)), nil
}

func readStamp(stampPath string) string {
	raw, err := os.ReadFile(stampPath)
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(raw))
}

// writeStamp records the inputs as they stand next to the file this run produced.
func writeStamp(rootDir, stampPath string, notices []byte) error {
	stamp, err := noticesStamp(rootDir, notices)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(stampPath), 0o755); err != nil {
		return fmt.Errorf("couldn't create %s: %w", filepath.Dir(stampPath), err)
	}
	return os.WriteFile(stampPath, []byte(stamp+"\n"), 0o644)
}

// --- installing cargo-about ---

// ensureCargoAbout returns the path to the pinned cargo-about, installing it when the binary is
// missing or reports a different version. Like `ensureCargoDeny`, it never trusts one on PATH: a
// different cargo-about harvests license files by its own rules, which would hand a local run a
// diff that CI can't reproduce.
func ensureCargoAbout(rootDir string) (string, error) {
	root := filepath.Join(rootDir, CargoToolsRootPath)
	bin := filepath.Join(root, "bin", "cargo-about")
	if output, err := RunCommand(exec.Command(bin, "--version"), true); err == nil &&
		strings.TrimSpace(output) == "cargo-about "+cargoAboutVersion {
		return bin, nil
	}

	installCmd := exec.Command("cargo", "install", "cargo-about", "--version", cargoAboutVersion,
		"--locked", "--features", "cli", "--root", root)
	if output, err := RunCommand(installCmd, true); err != nil {
		return "", fmt.Errorf("failed to install cargo-about %s: %w\n%s", cargoAboutVersion, err, indentOutput(output))
	}
	return bin, nil
}

// --- clarifications ---

// crateClarification pins which file(s) a crate's notice text is read from.
type crateClarification struct {
	crate string
	// license is the crate's own expression. A clarification REPLACES what cargo-about read from
	// the crate, so narrowing it here would make the notices claim a narrower license than
	// upstream offers.
	license string
	files   []clarifiedFile
}

// clarifiedFile is one pinned license file and the SPDX id of the text in it.
type clarifiedFile struct {
	path    string
	license string
}

// licenseClarifications pin the license text for crates whose manifest or files cargo-about
// can't read on its own.
//
// Checksums live in `clarificationChecksums` and are verified by `verifyClarifications` after the
// run, not trusted: cargo-about answers a stale checksum with a warning and a silent fall back to
// scanning.
var licenseClarifications = []crateClarification{
	// The MIT half covers code copied from `libm`, in a file cargo-about doesn't recognize, so it
	// fell back to the canonical MIT text and dropped libm's copyright line.
	{"dpi", "Apache-2.0 AND MIT", []clarifiedFile{{"LICENSE", "Apache-2.0"}, {"LICENSE-LIBM-MIT", "MIT"}}},
	// cargo-about found neither file and fell back to the canonical text. Both are pinned under
	// the LGPL because LGPL-3.0 is written as additional permissions on top of GPL-3.0, and its
	// section 4 asks that a copy of each travel with the combined work.
	{"lensfun", "LGPL-3.0-or-later", []clarifiedFile{
		{"LICENSE-LGPL-3.0", "LGPL-3.0-or-later"},
		{"LICENSE-GPL-3.0", "LGPL-3.0-or-later"},
	}},
	// `LICENSE` and `LICENSE-MIT.md` are the same text apart from one blank line, so cargo-about
	// takes both, in whatever order the filesystem lists them. Cmdr saw APFS and ext4 disagree.
	{"miniz_oxide", "MIT OR Zlib OR Apache-2.0", []clarifiedFile{{"LICENSE", "MIT"}}},
	// The manifest has `license-file = "LICENSE"` and no expression (`deny.toml` says the same).
	{"nom-exif", "MIT", []clarifiedFile{{"LICENSE", "MIT"}}},
	// The manifest says `LGPL-2.1`, an identifier SPDX deprecated (`deny.toml` reads it the same way).
	{"rawler", "LGPL-2.1-only", []clarifiedFile{{"LICENSE", "LGPL-2.1-only"}}},
}

// clarificationChecksums maps a clarified file to the sha256 cargo-about wants alongside it.
// Refresh one with:
//
//	shasum -a 256 ~/.cargo/registry/src/*/<crate>-<version>/<path>
var clarificationChecksums = map[string]string{
	"dpi/LICENSE":              "6dc0e068dcf3a5bc8e054205b85b7720e1d49265bbc64bf515d2cf79197df69a",
	"dpi/LICENSE-LIBM-MIT":     "9eff396a99bafd208e7126602420d6864f6066c08b84485a5b032320a9c01997",
	"lensfun/LICENSE-GPL-3.0":  "3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986",
	"lensfun/LICENSE-LGPL-3.0": "e3a994d82e644b03a792a930f574002658412f62407f5fee083f2555c5f23118",
	"miniz_oxide/LICENSE":      "4108245a1f2df9d4e94df8abed5b4ba0759bb2f9b40a6b939f1be141077ae50b",
	"nom-exif/LICENSE":         "9cfbbedc535b1a4b73b2f7f51fd683ac6cae914f6d4bc7f271b9b230dd4e2808",
	"rawler/LICENSE":           "4bb33cc4cd956b56b779b501f18cae46a9e26f8c8500cca86ed758b8bc5e1788",
}

// verifyClarifications fails when a pinned crate's text didn't come from exactly the pinned
// files.
//
// A clarification that's merely present proves nothing: cargo-about warns about a checksum
// mismatch, exits 0, and falls back to scanning the crate directory. So the result is what gets
// checked, and only a crate bump that rewrites one of these files can trip it. `sourcesByCrate`
// is keyed by "<name> <version>", so each version of a pinned crate is checked on its own.
func verifyClarifications(sourcesByCrate map[string][]string) error {
	for _, clarification := range licenseClarifications {
		expected := make([]string, 0, len(clarification.files))
		for _, file := range clarification.files {
			expected = append(expected, file.path)
		}
		sort.Strings(expected)
		// A crate missing from the shipped graph matches no key, so there's no text to have
		// come from the wrong file.
		for _, key := range slices.Sorted(maps.Keys(sourcesByCrate)) {
			if name, _, _ := strings.Cut(key, " "); name != clarification.crate {
				continue
			}
			actual := slices.Clone(sourcesByCrate[key])
			sort.Strings(actual)
			if !slices.Equal(expected, actual) {
				return fmt.Errorf(
					"the license pin for `%s` didn't take: its notice text came from %v, not the pinned %v.\n"+
						"cargo-about only warns when a pinned checksum goes stale, then falls back to scanning the crate.\n"+
						"A dependency bump probably rewrote the file. Refresh `clarificationChecksums` in\n"+
						"`scripts/check/checks/desktop-third-party-notices.go` with:\n"+
						"  shasum -a 256 ~/.cargo/registry/src/*/%s-*/<path>",
					clarification.crate, actual, expected, clarification.crate)
			}
		}
	}
	return nil
}

// --- cargo-about ---

// attributedPackage is one shipped crate, as the notices list it.
//
// **No version, on purpose.** None of these licenses asks for one, and leaving it out is what
// lets a patch bump regenerate a byte-identical file. With versions, every lockfile change made
// the committed copy stale, so a Renovate PR could never pass CI on its own: that's what pushed
// Cmdr's desktop dependencies off automerge. Now only a bump that adds or drops a crate, or
// changes a license text, needs a local run.
type attributedPackage struct {
	Name    string
	License string
	URL     string
}

// licenseText is one distinct license text plus who it covers. Distinct means by content: most
// MIT texts differ only in their copyright line, and reproducing each is the point of the file.
type licenseText struct {
	ID   string
	Text string
	// SourcePath is the file the text was read from, relative to the crate root. It goes into the
	// notices so a crate that starts reporting a different license file shows up as a readable
	// diff line rather than a count that moved for no visible reason.
	SourcePath string
	UsedBy     []string
	sortKey    string
}

type rustCollection struct {
	packages []attributedPackage
	texts    []licenseText
}

// crateNames is the set of crate names in the shipped graph.
func (rust rustCollection) crateNames() map[string]bool {
	names := map[string]bool{}
	for _, pkg := range rust.packages {
		names[pkg.Name] = true
	}
	return names
}

type aboutPackage struct {
	Name       string `json:"name"`
	Version    string `json:"version"`
	Repository string `json:"repository"`
	Homepage   string `json:"homepage"`
}

type aboutOutput struct {
	Licenses []struct {
		ID   string `json:"id"`
		Text string `json:"text"`
		// SourcePath is absolute for a crate cargo-about scanned, crate-relative for one a
		// clarification pinned, and absent for a canonical-text fallback.
		SourcePath string `json:"source_path"`
		UsedBy     []struct {
			Crate aboutPackage `json:"crate"`
		} `json:"used_by"`
	} `json:"licenses"`
	Crates []struct {
		Package aboutPackage `json:"package"`
		License string       `json:"license"`
	} `json:"crates"`
}

// collectRustCrates runs cargo-about over the app crate and reads back what it found.
func collectRustCrates(rootDir, bin string) (rustCollection, error) {
	workDir, err := os.MkdirTemp("", "prvw-notices-")
	if err != nil {
		return rustCollection{}, fmt.Errorf("couldn't create a temp dir: %w", err)
	}
	defer func() { _ = os.RemoveAll(workDir) }()

	configPath := filepath.Join(workDir, "about.toml")
	config, err := renderAboutConfig(rootDir)
	if err != nil {
		return rustCollection{}, err
	}
	if err := os.WriteFile(configPath, []byte(config), 0o644); err != nil {
		return rustCollection{}, fmt.Errorf("couldn't write the cargo-about config: %w", err)
	}

	outPath := filepath.Join(workDir, "about.json")
	// Run straight from the binary rather than as `cargo about`, so no other cargo-about on PATH
	// can stand in for the pinned one. Cargo would pass the subcommand name as the first
	// argument, so it's given here by hand.
	cmd := exec.Command(bin, "about", "generate", "-c", configPath, "--format", "json", "-o", outPath, "--fail")
	cmd.Dir = filepath.Join(rootDir, "apps", "desktop")
	if output, err := RunCommand(cmd, true); err != nil {
		return rustCollection{}, fmt.Errorf(
			"`cargo about generate` failed. A crate may carry a license that `deny.toml` doesn't allow, or one"+
				" cargo-about can't read, which wants a clarification in this check\n%s", indentOutput(output))
	}

	raw, err := os.ReadFile(outPath)
	if err != nil {
		return rustCollection{}, fmt.Errorf("couldn't read cargo-about output: %w", err)
	}
	var parsed aboutOutput
	if err := json.Unmarshal(raw, &parsed); err != nil {
		return rustCollection{}, fmt.Errorf("couldn't parse cargo-about output: %w", err)
	}
	return collectionFromAbout(parsed)
}

// collectionFromAbout turns cargo-about's report into the sorted lists the notices print.
func collectionFromAbout(parsed aboutOutput) (rustCollection, error) {
	collection := rustCollection{}
	for _, crate := range parsed.Crates {
		pkg := attributedPackage{
			Name:    crate.Package.Name,
			License: crate.License,
			URL:     firstNonEmpty(crate.Package.Repository, crate.Package.Homepage),
		}
		// Two versions of one crate are one line, unless they disagree about the license.
		if !slices.Contains(collection.packages, pkg) {
			collection.packages = append(collection.packages, pkg)
		}
	}
	sourcesByCrate := map[string][]string{}
	for _, license := range parsed.Licenses {
		users := make([]string, 0, len(license.UsedBy))
		source := crateRelativeSource(license.SourcePath)
		for _, used := range license.UsedBy {
			// The pin check still tells versions apart, since a bump can rewrite one file.
			key := used.Crate.Name + " " + used.Crate.Version
			sourcesByCrate[key] = append(sourcesByCrate[key], source)
			if !slices.Contains(users, used.Crate.Name) {
				users = append(users, used.Crate.Name)
			}
		}
		sort.Strings(users)
		collection.texts = append(collection.texts, licenseText{
			ID: license.ID,
			// Some upstream license files ship CRLF, and git would rewrite them to LF on
			// checkout: the committed file would then never match a fresh regeneration.
			Text:       normalizeNewlines(license.Text),
			SourcePath: source,
			UsedBy:     users,
			sortKey:    strings.Join([]string{license.ID, strings.Join(users, ","), source, license.Text}, "\x00"),
		})
	}

	if err := verifyClarifications(sourcesByCrate); err != nil {
		return rustCollection{}, err
	}

	sortPackages(collection.packages)
	sort.Slice(collection.texts, func(i, j int) bool {
		return collection.texts[i].sortKey < collection.texts[j].sortKey
	})
	return collection, nil
}

// registrySrcMarker is the path segment that precedes `<index>/<crate>-<version>/` in an
// extracted registry crate's location.
const registrySrcMarker = "/registry/src/"

// crateRelativeSource turns cargo-about's absolute license-file path into one that means the same
// thing on every machine: relative to the crate root. The raw path would bake `/Users/<whoever>`
// and a registry index hash into a committed file. A clarified crate already reports a
// crate-relative path, and a canonical-text fallback reports none.
func crateRelativeSource(sourcePath string) string {
	slashed := filepath.ToSlash(sourcePath)
	if slashed == "" || !strings.HasPrefix(slashed, "/") && !filepath.IsAbs(sourcePath) {
		return slashed
	}
	if index := strings.Index(slashed, registrySrcMarker); index >= 0 {
		// `<index>/<crate>-<version>/<path within the crate>`
		if parts := strings.SplitN(slashed[index+len(registrySrcMarker):], "/", 3); len(parts) == 3 {
			return parts[2]
		}
	}
	return filepath.Base(sourcePath)
}

// renderAboutConfig writes cargo-about's config, with every policy decision read from
// `deny.toml`.
//
// Single-sourced on purpose: `deny.toml` already decides which licenses Prvw may depend on and
// which platforms ship, and a second hand-kept list here would answer the same questions
// differently sooner or later. The accepted list is `[licenses] allow` in its own order, which
// is also cargo-about's preference order for an `OR` expression, then the licenses the
// per-crate exceptions allow. BUSL-1.1 is left out: it's our own, and our crates are
// `publish = false`, which `[private] ignore` drops.
func renderAboutConfig(rootDir string) (string, error) {
	denyRaw, err := os.ReadFile(filepath.Join(rootDir, "deny.toml"))
	if err != nil {
		return "", fmt.Errorf("couldn't read deny.toml: %w", err)
	}
	deny := string(denyRaw)

	accepted := tomlStringArray(deny, "allow")
	if len(accepted) == 0 {
		return "", fmt.Errorf("no `[licenses] allow = [...]` found in deny.toml")
	}
	for _, id := range denyExceptionLicenses(deny) {
		if id != "BUSL-1.1" && !slices.Contains(accepted, id) {
			accepted = append(accepted, id)
		}
	}
	targets := tomlStringArray(deny, "targets")
	if len(targets) == 0 {
		return "", fmt.Errorf("no `[graph] targets = [...]` found in deny.toml")
	}

	return fmt.Sprintf(`# Generated by the `+"`third-party-notices`"+` check from deny.toml. See desktop-third-party-notices.go.
accepted = [%s]
targets = [%s]
ignore-dev-dependencies = true

[private]
ignore = true
%s`, quoteAll(accepted), quoteAll(targets), renderClarifications()), nil
}

// renderClarifications emits the `[<crate>.clarify]` sections. A file with no entry in
// `clarificationChecksums` is written without one, which cargo-about rejects outright: better a
// hard failure naming the crate than a silent fall back to scanning it.
func renderClarifications() string {
	var b strings.Builder
	for _, clarification := range licenseClarifications {
		fmt.Fprintf(&b, "\n[%s.clarify]\nlicense = %q\n", clarification.crate, clarification.license)
		for _, file := range clarification.files {
			fmt.Fprintf(&b, "\n[[%s.clarify.files]]\npath = %q\nlicense = %q\nchecksum = %q\n",
				clarification.crate, file.path, file.license,
				clarificationChecksums[clarification.crate+"/"+file.path])
		}
	}
	return b.String()
}

func quoteAll(values []string) string {
	quoted := make([]string, 0, len(values))
	for _, value := range values {
		quoted = append(quoted, fmt.Sprintf("%q", value))
	}
	return strings.Join(quoted, ", ")
}

// quotedString matches one double-quoted TOML string.
var quotedString = regexp.MustCompile(`"([^"]*)"`)

// tomlStringArray reads the first multi-line `key = [ ... ]` array of strings in a TOML file.
// A line scan rather than a TOML dependency: `deny.toml` keeps both arrays one quoted value per
// line, with comment lines between them, and a scan that finds nothing fails loudly at the call
// site.
func tomlStringArray(contents, key string) []string {
	var values []string
	inArray := false
	for line := range strings.SplitSeq(contents, "\n") {
		trimmed := strings.TrimSpace(line)
		if !inArray {
			if strings.HasPrefix(trimmed, key+" = [") && !strings.Contains(trimmed, "]") {
				inArray = true
			}
			continue
		}
		if strings.HasPrefix(trimmed, "]") {
			break
		}
		if strings.HasPrefix(trimmed, "#") {
			continue
		}
		if match := quotedString.FindStringSubmatch(trimmed); match != nil {
			values = append(values, match[1])
		}
	}
	return values
}

// exceptionAllow matches one `allow = [...]` inside an `[licenses] exceptions` inline table.
var exceptionAllow = regexp.MustCompile(`^\{\s*crate\s*=\s*"[^"]+"\s*,\s*allow\s*=\s*\[([^\]]*)\]`)

// denyExceptionLicenses lists the licenses `deny.toml`'s per-crate exceptions allow, in order.
func denyExceptionLicenses(contents string) []string {
	var ids []string
	for line := range strings.SplitSeq(contents, "\n") {
		match := exceptionAllow.FindStringSubmatch(strings.TrimSpace(line))
		if match == nil {
			continue
		}
		for _, quoted := range quotedString.FindAllStringSubmatch(match[1], -1) {
			if !slices.Contains(ids, quoted[1]) {
				ids = append(ids, quoted[1])
			}
		}
	}
	return ids
}

// --- rendering ---

// lgplNote is what the LGPL asks of a binary that links an LGPL library statically: that the
// person who has it can rebuild it against a modified copy. Prvw's public source is what makes
// that possible, so the file says where it is.
const lgplNote = "Prvw links %s statically. Their licenses (the GNU LGPL) let you replace them with a\n" +
	"modified version: Prvw's own source is public at <https://github.com/vdavid/prvw>, so you can\n" +
	"rebuild the app against your copy. Their own sources are linked in the list below.\n\n"

func renderNotices(rust rustCollection, vendored []vendoredCredit, vendoredTexts map[string]string) []byte {
	var b strings.Builder

	b.WriteString("# Third-party notices\n\n")
	b.WriteString("Prvw is built on open-source software and open data. This file lists every third-party crate and\n")
	b.WriteString("data set that ships inside the app, with its license, and reproduces the license texts in full.\n\n")
	b.WriteString("Generated by `./scripts/check.sh --check third-party-notices` from `Cargo.lock`, `deny.toml`, and\n")
	b.WriteString("the hand-kept `" + vendoredCreditsPath + "`. Don't edit it by hand; edit\n")
	b.WriteString("that list or the generator at `scripts/check/checks/desktop-third-party-notices.go` instead.\n\n")

	fmt.Fprintf(&b, "- Rust crates: %d\n", len(rust.packages))
	fmt.Fprintf(&b, "- Data and other files: %d\n", len(vendored))
	fmt.Fprintf(&b, "- Distinct license texts: %d\n\n", len(rust.texts)+len(vendoredTexts))

	if lgpl := lgplCrates(rust.packages); len(lgpl) > 0 {
		fmt.Fprintf(&b, lgplNote, joinNames(lgpl))
	}

	writeVendoredSection(&b, vendored, vendoredTexts)
	writePackageSection(&b, "Rust crates", rust.packages)

	b.WriteString("## License texts\n\n")
	b.WriteString("Each distinct text is reproduced once, with the crates it covers and the file it was read from.\n")
	b.WriteString("Many differ only in their copyright line, which is exactly what these licenses require to be\n")
	b.WriteString("carried along.\n\n")
	for _, text := range rust.texts {
		fmt.Fprintf(&b, "### %s\n\n", text.ID)
		fmt.Fprintf(&b, "Covers: %s\n\n", strings.Join(text.UsedBy, ", "))
		if text.SourcePath != "" {
			fmt.Fprintf(&b, "Text from: `%s`\n\n", text.SourcePath)
		}
		writeTextBlock(&b, text.Text)
	}

	return []byte(b.String())
}

// writeTextBlock fences a license text. Four backticks, because a few license files carry a
// three-backtick fence of their own.
func writeTextBlock(b *strings.Builder, text string) {
	b.WriteString("````text\n")
	b.WriteString(strings.TrimRight(text, "\n"))
	b.WriteString("\n````\n\n")
}

// lgplCrates names the shipped crates under an LGPL, as `name` once per crate.
func lgplCrates(packages []attributedPackage) []string {
	var names []string
	for _, pkg := range packages {
		if strings.Contains(pkg.License, "LGPL") && !slices.Contains(names, pkg.Name) {
			names = append(names, pkg.Name)
		}
	}
	return names
}

// joinNames renders `a`, `a and b`, or `a, b, and c`, each in backticks.
func joinNames(names []string) string {
	quoted := make([]string, len(names))
	for i, name := range names {
		quoted[i] = "`" + name + "`"
	}
	switch len(quoted) {
	case 1:
		return quoted[0]
	case 2:
		return quoted[0] + " and " + quoted[1]
	}
	return strings.Join(quoted[:len(quoted)-1], ", ") + ", and " + quoted[len(quoted)-1]
}

func writePackageSection(b *strings.Builder, title string, packages []attributedPackage) {
	fmt.Fprintf(b, "## %s\n\n", title)
	for _, pkg := range packages {
		fmt.Fprintf(b, "- **%s**, %s", pkg.Name, pkg.License)
		if pkg.URL != "" {
			fmt.Fprintf(b, ", <%s>", pkg.URL)
		}
		b.WriteString("\n")
	}
	b.WriteString("\n")
}

func sortPackages(packages []attributedPackage) {
	sort.Slice(packages, func(i, j int) bool {
		if packages[i].Name != packages[j].Name {
			return strings.ToLower(packages[i].Name) < strings.ToLower(packages[j].Name)
		}
		return packages[i].License < packages[j].License
	})
}

// normalizeNewlines makes the generated file LF-only, so what git stores and what a regeneration
// produces can't disagree.
func normalizeNewlines(text string) string {
	return strings.ReplaceAll(strings.ReplaceAll(text, "\r\n", "\n"), "\r", "\n")
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}
	return ""
}
