package checks

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path"
	"path/filepath"
	"slices"
	"sort"
	"strings"
)

// vendoredCreditsPath is the hand-kept list of third-party material Prvw ships that no license
// field describes: data committed to this repo (the DCP camera profiles), and data a crate
// compiles in under a license of its own (lensfun's lens database, which is CC BY-SA while the
// crate is LGPL). `third-party-notices` validates it and merges it into THIRD-PARTY-NOTICES.md.
// Repo-relative and slash-separated.
const vendoredCreditsPath = "scripts/check/checks/third-party-vendored.json"

// vendoredCredit is one entry of the vendored list. It asks for more than a crate carries,
// because the licenses involved do: CC BY-SA wants the author, the source, a link to the
// license, and whether the material was changed.
type vendoredCredit struct {
	Name string `json:"name"`
	// Version is optional: a data set copied out of a repo rarely has one.
	Version    string `json:"version,omitempty"`
	Author     string `json:"author"`
	License    string `json:"license"`
	LicenseURL string `json:"license_url"`
	// LicenseFile is a repo file whose text the notices reproduce in full. Required for a
	// `LicenseRef-` license, which has no canonical text to point at.
	LicenseFile string `json:"license_file,omitempty"`
	// URL is where the material comes from.
	URL string `json:"url"`
	// Changes says what changed, or that nothing did. Required either way, so "unchanged" is a
	// statement rather than a forgotten field.
	Changes string `json:"changes"`
	// Files are the repo-relative files or folders the credit covers. The check fails when one no
	// longer exists, so a credit can't outlive what it credits.
	Files []string `json:"files,omitempty"`
	// Crate names the dependency the material ships inside. The check fails when it's no longer
	// in the shipped graph, for the same reason.
	Crate string `json:"crate,omitempty"`
}

// vendoredLicenses are the SPDX ids a vendored credit may carry, besides a `LicenseRef-` with a
// `license_file`. The point is catching the spelling a README uses (`CC-BY-SA 3.0`), which no
// SPDX tooling reads; add an id here when material under a new license ships.
var vendoredLicenses = []string{"Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "CC-BY-4.0", "CC-BY-SA-3.0", "CC0-1.0", "ISC", "MIT", "OFL-1.1"}

// loadVendoredCredits reads, sorts, and validates the vendored list. `shippedCrates` is the set
// of crate names in the shipped graph, which a `crate` field has to name.
func loadVendoredCredits(rootDir string, shippedCrates map[string]bool) ([]vendoredCredit, error) {
	raw, err := os.ReadFile(filepath.Join(rootDir, filepath.FromSlash(vendoredCreditsPath)))
	if err != nil {
		return nil, fmt.Errorf("couldn't read %s: %w", vendoredCreditsPath, err)
	}
	credits, err := parseVendoredCredits(raw)
	if err != nil {
		return nil, err
	}
	if err := validateVendoredCredits(credits, rootDir, shippedCrates); err != nil {
		return nil, err
	}
	return credits, nil
}

// parseVendoredCredits decodes the list and sorts it by name, so the generated file doesn't
// depend on the order it's kept in. An unknown field fails: a misspelled `licence_url` would
// otherwise decode as an empty field, and the failure would report a missing link where the
// problem is a typo.
func parseVendoredCredits(raw []byte) ([]vendoredCredit, error) {
	var parsed struct {
		Comment string           `json:"comment"`
		Credits []vendoredCredit `json:"credits"`
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&parsed); err != nil {
		return nil, fmt.Errorf("couldn't parse %s: %w", vendoredCreditsPath, err)
	}
	sort.SliceStable(parsed.Credits, func(i, j int) bool {
		return strings.ToLower(parsed.Credits[i].Name) < strings.ToLower(parsed.Credits[j].Name)
	})
	return parsed.Credits, nil
}

// validateVendoredCredits reports every problem in the list at once, so one run names
// everything to fix.
func validateVendoredCredits(credits []vendoredCredit, rootDir string, shippedCrates map[string]bool) error {
	var problems []string
	seen := map[string]bool{}
	for index, credit := range credits {
		label := credit.Name
		if label == "" {
			label = fmt.Sprintf("credit #%d", index+1)
		}
		if credit.Name != "" {
			key := strings.ToLower(credit.Name)
			if seen[key] {
				problems = append(problems, label+": another credit has the same name")
			}
			seen[key] = true
		}
		for _, problem := range creditProblems(credit, rootDir, shippedCrates) {
			problems = append(problems, label+": "+problem)
		}
	}
	if len(problems) == 0 {
		return nil
	}
	return fmt.Errorf("%s has %d problem(s):\n%s", vendoredCreditsPath, len(problems), indentOutput(strings.Join(problems, "\n")))
}

// creditProblems is what's wrong with one credit on its own terms.
func creditProblems(credit vendoredCredit, rootDir string, shippedCrates map[string]bool) []string {
	var problems []string
	for _, field := range []struct{ name, value string }{
		{"name", credit.Name},
		{"author", credit.Author},
		{"license", credit.License},
		{"license_url", credit.LicenseURL},
		{"url", credit.URL},
		{"changes", credit.Changes},
	} {
		if strings.TrimSpace(field.value) == "" {
			problems = append(problems, fmt.Sprintf("`%s` is empty", field.name))
		}
	}
	problems = append(problems, licenseProblems(credit, rootDir)...)
	if len(credit.Files) == 0 && credit.Crate == "" {
		problems = append(problems, "neither `files` nor `crate` is set, so nothing says what the credit covers")
	}
	for _, file := range credit.Files {
		if problem := creditedPathProblem(file, rootDir, false); problem != "" {
			problems = append(problems, problem)
		}
	}
	if credit.Crate != "" && !shippedCrates[credit.Crate] {
		problems = append(problems, fmt.Sprintf("`%s` isn't in the shipped dependency graph (removed or renamed?)", credit.Crate))
	}
	return problems
}

// licenseProblems checks the license id, and that a `LicenseRef-` brings its own text.
func licenseProblems(credit vendoredCredit, rootDir string) []string {
	var problems []string
	isRef := strings.HasPrefix(credit.License, "LicenseRef-")
	if credit.License != "" && !isRef && !slices.Contains(vendoredLicenses, credit.License) {
		problems = append(problems, fmt.Sprintf("`%s` isn't a known SPDX id (%s) or a `LicenseRef-`", credit.License, strings.Join(vendoredLicenses, ", ")))
	}
	if isRef && credit.LicenseFile == "" {
		problems = append(problems, "a `LicenseRef-` license needs a `license_file` with its text")
	}
	if credit.LicenseFile != "" {
		if problem := creditedPathProblem(credit.LicenseFile, rootDir, true); problem != "" {
			problems = append(problems, problem)
		}
	}
	return problems
}

// creditedPathProblem says why a credited path doesn't hold up, or returns "". A folder is fine
// for `files` (the DCP collection is 161 profiles); a license text has to be one file.
func creditedPathProblem(file, rootDir string, mustBeFile bool) string {
	if file != path.Clean(file) || !filepath.IsLocal(filepath.FromSlash(file)) {
		return fmt.Sprintf("`%s` isn't a clean repo-relative path", file)
	}
	info, err := os.Stat(filepath.Join(rootDir, filepath.FromSlash(file)))
	switch {
	case err != nil:
		return fmt.Sprintf("`%s` isn't in the repo (renamed or deleted?)", file)
	case mustBeFile && !info.Mode().IsRegular():
		return fmt.Sprintf("`%s` isn't a file", file)
	case !info.Mode().IsRegular() && !info.IsDir():
		return fmt.Sprintf("`%s` is neither a file nor a folder", file)
	}
	return ""
}

// writeVendoredSection credits each work with everything its license asks an attribution to
// carry: the author, the source, a link to the license, what changed, and where it ships.
func writeVendoredSection(b *strings.Builder, credits []vendoredCredit, licenseTexts map[string]string) {
	b.WriteString("## Data and other files\n\n")
	for _, credit := range credits {
		fmt.Fprintf(b, "- **%s**", credit.Name)
		if credit.Version != "" {
			fmt.Fprintf(b, " %s", credit.Version)
		}
		fmt.Fprintf(b, " by %s, %s, <%s>\n", credit.Author, credit.License, credit.URL)
		fmt.Fprintf(b, "  - License: <%s>\n", credit.LicenseURL)
		fmt.Fprintf(b, "  - Changes: %s\n", credit.Changes)
		if len(credit.Files) > 0 {
			fmt.Fprintf(b, "  - Files: `%s`\n", strings.Join(credit.Files, "`, `"))
		}
		if credit.Crate != "" {
			fmt.Fprintf(b, "  - Ships inside: the `%s` crate\n", credit.Crate)
		}
	}
	b.WriteString("\n")
	for _, credit := range credits {
		text, ok := licenseTexts[credit.Name]
		if !ok {
			continue
		}
		fmt.Fprintf(b, "### %s license\n\n", credit.Name)
		fmt.Fprintf(b, "Text from: `%s`\n\n", credit.LicenseFile)
		writeTextBlock(b, text)
	}
}

// readVendoredLicenseTexts loads each `license_file`, keyed by credit name.
func readVendoredLicenseTexts(rootDir string, credits []vendoredCredit) (map[string]string, error) {
	texts := map[string]string{}
	for _, credit := range credits {
		if credit.LicenseFile == "" {
			continue
		}
		raw, err := os.ReadFile(filepath.Join(rootDir, filepath.FromSlash(credit.LicenseFile)))
		if err != nil {
			return nil, fmt.Errorf("couldn't read %s: %w", credit.LicenseFile, err)
		}
		texts[credit.Name] = normalizeNewlines(string(raw))
	}
	return texts, nil
}
