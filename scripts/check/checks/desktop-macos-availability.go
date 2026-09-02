package checks

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
)

// This check catches the class of bug that shipped in v0.15.1: `NSBezierPath`'s
// `curveToPoint:controlPoint:` is macOS 14+, and calling it on macOS 13 aborts the
// process with "unrecognized selector sent to instance". Nothing in the Rust
// toolchain catches that: `objc2` bindings carry no availability data, and the
// linker never sees an ObjC selector.
//
// So we read the availability annotations straight from the macOS SDK headers and
// compare them against every AppKit-ish call site in the desktop app. The floor
// comes from `LSMinimumSystemVersion` in `apps/desktop/Info.plist`, so raising or
// lowering what we claim to support automatically re-scopes the check.

// macOSVersion is a two-component macOS version. Patch levels never matter for
// availability annotations.
type macOSVersion struct {
	major int
	minor int
}

func (v macOSVersion) String() string { return fmt.Sprintf("%d.%d", v.major, v.minor) }

func (v macOSVersion) newerThan(other macOSVersion) bool {
	if v.major != other.major {
		return v.major > other.major
	}
	return v.minor > other.minor
}

// parseMacOSVersion accepts both annotation spellings: "14.0" (API_AVAILABLE) and
// "10_15" (the older NS_AVAILABLE_MAC macros).
func parseMacOSVersion(s string) (macOSVersion, bool) {
	parts := strings.Split(strings.ReplaceAll(s, "_", "."), ".")
	major, err := strconv.Atoi(parts[0])
	if err != nil {
		return macOSVersion{}, false
	}
	minor := 0
	if len(parts) > 1 {
		minor, _ = strconv.Atoi(parts[1])
	}
	return macOSVersion{major, minor}, true
}

// runtimeGatedClasses are classes newer than the floor that we deliberately use
// behind a runtime existence check, so they never reach an older machine. Each
// entry names the gate so a reader can verify it still exists.
var runtimeGatedClasses = map[string]string{
	"NSGlassEffectView": "window::liquid_glass_available() (objc_getClass probe)",
}

// selectorsAssumedOld exempts names that look like a too-new selector but aren't
// a selector at all. A zero-argument ObjC selector is a bare word, so it can
// collide with an ordinary Rust method of the same name; multi-part selectors
// carry underscores and effectively never collide. Add an entry only after
// confirming the call site is Rust, and say which one.
var selectorsAssumedOld = map[string]string{
	"current": "`DirList::current()` in app.rs, not `NSAppearance.current` (macOS 11)",
}

var (
	availabilityMacroRe = regexp.MustCompile(`(?:API_AVAILABLE|API_DEPRECATED|API_DEPRECATED_WITH_REPLACEMENT)\s*\([^)]*macos\(([0-9][0-9._]*)\)`)
	nsAvailabilityRe    = regexp.MustCompile(`NS_(?:AVAILABLE_MAC|CLASS_AVAILABLE_MAC|DEPRECATED_MAC)\s*\(\s*([0-9][0-9._]*)`)
	selectorPartRe      = regexp.MustCompile(`([A-Za-z_][A-Za-z0-9_]*)\s*:`)
	zeroArgSelectorRe   = regexp.MustCompile(`^[-+]\s*\([^)]*\)\s*([A-Za-z_][A-Za-z0-9_]*)`)
	interfaceRe         = regexp.MustCompile(`^@interface\s+([A-Za-z_][A-Za-z0-9_]*)\s*(\()?`)
	// A bare annotation macro on its own line, decorating whatever declaration follows.
	standaloneAnnotationRe = regexp.MustCompile(`^(?:API_AVAILABLE|API_DEPRECATED|NS_CLASS_AVAILABLE_MAC|NS_AVAILABLE_MAC|NS_CLASS_DEPRECATED_MAC)\s*\(`)
	propertyGetterRe       = regexp.MustCompile(`getter\s*=\s*([A-Za-z_][A-Za-z0-9_]*)`)
	propertySetterRe       = regexp.MustCompile(`setter\s*=\s*([A-Za-z_][A-Za-z0-9_:]*)`)
	propertyNameRe         = regexp.MustCompile(`([A-Za-z_][A-Za-z0-9_]*)\s*(?:API_|NS_|CF_|CG_|__|;)`)

	// objc2 renders `curveToPoint:controlPoint:` as `curveToPoint_controlPoint`,
	// and a no-argument selector as its bare name.
	objcCallRe  = regexp.MustCompile(`\.([a-z][a-zA-Z0-9]*(?:_[A-Za-z0-9]+)*)\s*\(`)
	objcClassRe = regexp.MustCompile(`\b((?:NS|QL|CG|CF|CA|MTL|UT)[A-Z][A-Za-z0-9]*)\b`)

	minSystemVersionRe = regexp.MustCompile(`(?s)<key>LSMinimumSystemVersion</key>\s*<string>([^<]+)</string>`)

	// Annotation macros trail the selector; everything from the first one on is
	// noise for selector reconstruction.
	annotationCuts = []string{"API_AVAILABLE", "API_DEPRECATED", "API_UNAVAILABLE", "NS_AVAILABLE", "NS_DEPRECATED", "NS_SWIFT", "NS_REFINED", "__attribute__"}
)

// availabilityFinding is one call site that needs a newer macOS than we claim.
type availabilityFinding struct {
	file    string
	line    int
	symbol  string
	version macOSVersion
	kind    string // "selector" or "class"
}

// RunMacOSAvailability compares every ObjC selector and class the desktop app
// touches against the SDK's availability annotations, and fails when one needs a
// newer macOS than `LSMinimumSystemVersion` promises.
func RunMacOSAvailability(ctx *CheckContext) (CheckResult, error) {
	sdkPath, err := macOSSDKPath()
	if err != nil {
		return Skipped("no macOS SDK found (xcrun unavailable)"), nil
	}

	floor, err := deploymentFloor(ctx.RootDir)
	if err != nil {
		return CheckResult{}, err
	}

	index, err := buildSDKAvailabilityIndex(sdkPath)
	if err != nil {
		return CheckResult{}, err
	}
	if len(index.selectors) == 0 {
		return CheckResult{}, fmt.Errorf("parsed no selectors from %s — the header layout changed, so this check is blind", sdkPath)
	}

	srcDir := filepath.Join(ctx.RootDir, "apps", "desktop", "src")
	findings, scanned, err := scanRustForNewerAPIs(srcDir, index, floor)
	if err != nil {
		return CheckResult{}, err
	}

	if len(findings) > 0 {
		return CheckResult{}, formatAvailabilityError(findings, floor)
	}

	result := Success(fmt.Sprintf("%d %s clean against macOS %s floor",
		scanned, Pluralize(scanned, "call site", "call sites"), floor))
	result.Total = scanned
	result.Issues = 0
	return result, nil
}

// macOSSDKPath asks xcrun where the current SDK lives.
func macOSSDKPath() (string, error) {
	out, err := exec.Command("xcrun", "--show-sdk-path").Output()
	if err != nil {
		return "", err
	}
	path := strings.TrimSpace(string(out))
	if path == "" {
		return "", fmt.Errorf("xcrun returned an empty SDK path")
	}
	return path, nil
}

// deploymentFloor reads the oldest macOS we claim to run on.
func deploymentFloor(rootDir string) (macOSVersion, error) {
	path := filepath.Join(rootDir, "apps", "desktop", "Info.plist")
	data, err := os.ReadFile(path)
	if err != nil {
		return macOSVersion{}, fmt.Errorf("failed to read Info.plist: %w", err)
	}
	m := minSystemVersionRe.FindSubmatch(data)
	if m == nil {
		return macOSVersion{}, fmt.Errorf("no LSMinimumSystemVersion in %s", path)
	}
	v, ok := parseMacOSVersion(string(m[1]))
	if !ok {
		return macOSVersion{}, fmt.Errorf("unparseable LSMinimumSystemVersion %q in %s", m[1], path)
	}
	return v, nil
}

// sdkAvailabilityIndex maps ObjC names to the oldest macOS that declares them.
type sdkAvailabilityIndex struct {
	selectors map[string]macOSVersion
	classes   map[string]macOSVersion
	headers   int
}

// note records the OLDEST version seen for a name. A selector spelled the same on
// an ancient class and a new one counts as ancient, which keeps false positives
// down at the cost of missing same-name collisions.
func note(m map[string]macOSVersion, key string, v macOSVersion) {
	if cur, ok := m[key]; !ok || cur.newerThan(v) {
		m[key] = v
	}
}

// buildSDKAvailabilityIndex walks every framework header in the SDK and records
// the availability annotation attached to each class, method, and property.
func buildSDKAvailabilityIndex(sdkPath string) (sdkAvailabilityIndex, error) {
	index := sdkAvailabilityIndex{
		selectors: map[string]macOSVersion{},
		classes:   map[string]macOSVersion{},
	}

	dirs, err := filepath.Glob(filepath.Join(sdkPath, "System/Library/Frameworks/*.framework/Headers"))
	if err != nil {
		return index, fmt.Errorf("failed to list SDK frameworks: %w", err)
	}
	for _, dir := range dirs {
		// Every `X.framework/Headers` in the SDK is a symlink into `Versions/Current`,
		// and filepath.Walk lstats its root, so walking the link itself yields nothing.
		resolved, err := filepath.EvalSymlinks(dir)
		if err != nil {
			continue
		}
		err = filepath.Walk(resolved, func(path string, info os.FileInfo, err error) error {
			if err != nil || info.IsDir() || !strings.HasSuffix(path, ".h") {
				return nil //nolint:nilerr // an unreadable header is not worth failing the run
			}
			data, err := os.ReadFile(path)
			if err != nil {
				return nil
			}
			index.headers++
			indexHeader(string(data), &index)
			return nil
		})
		if err != nil {
			return index, fmt.Errorf("failed to walk %s: %w", resolved, err)
		}
	}
	return index, nil
}

// headerScanner carries the parse state that spans lines within one header.
type headerScanner struct {
	index *sdkAvailabilityIndex

	// pending holds an availability macro found on its own line, waiting for the
	// declaration below it (that is how `NSGlassEffectView`'s macos(26.0) is
	// written). Blank and comment lines preserve it; anything else drops it.
	pending *macOSVersion

	// enclosing is the floor inherited from the current `@interface`. A method on
	// a macOS 26 class needs macOS 26 whether or not it says so itself.
	enclosing macOSVersion
}

// indexHeader pulls class, method, and property declarations out of one header.
func indexHeader(content string, index *sdkAvailabilityIndex) {
	scanner := &headerScanner{index: index, enclosing: macOSVersion{10, 0}}
	lines := strings.Split(content, "\n")
	for i := 0; i < len(lines); i++ {
		i = scanner.consumeLine(lines, i)
	}
}

// consumeLine handles the line at i and returns the index of the last line it
// consumed, so a declaration wrapped across lines advances the caller's cursor.
func (s *headerScanner) consumeLine(lines []string, i int) int {
	line := strings.TrimSpace(lines[i])

	switch {
	case strings.HasPrefix(line, "@end"):
		s.enclosing, s.pending = macOSVersion{10, 0}, nil
	case interfaceRe.MatchString(line):
		s.consumeInterface(line)
	case isDeclarationStart(line):
		decl, last := joinDeclaration(lines, i)
		s.consumeDeclaration(decl, strings.HasPrefix(line, "@property"))
		return last
	default:
		s.consumeFiller(line)
	}
	return i
}

// consumeInterface records a class and opens its inheritance scope.
func (s *headerScanner) consumeInterface(line string) {
	m := interfaceRe.FindStringSubmatch(line)
	version := declaredVersionWith(line, s.pending)
	s.pending = nil

	// `@interface NSView (SomeCategory)` carries no availability of its own, so
	// recording it would reset the class to "always available".
	if m[2] != "(" {
		note(s.index.classes, m[1], version)
		s.enclosing = version
		return
	}
	if known, ok := s.index.classes[m[1]]; ok {
		s.enclosing = known
	}
}

// consumeDeclaration indexes one method or property declaration.
func (s *headerScanner) consumeDeclaration(decl string, isProperty bool) {
	version := declaredVersionWith(decl, s.pending)
	if s.enclosing.newerThan(version) {
		version = s.enclosing
	}
	s.pending = nil

	if isProperty {
		indexProperty(decl, version, s.index)
		return
	}
	if sel := selectorFromDeclaration(decl); sel != "" {
		note(s.index.selectors, sel, version)
	}
}

// consumeFiller handles everything that is not a declaration: it picks up a
// standalone annotation, preserves it across comments and blank lines, and drops
// it at any other content.
func (s *headerScanner) consumeFiller(line string) {
	if line == "" || strings.HasPrefix(line, "//") || strings.HasPrefix(line, "*") || strings.HasPrefix(line, "/*") {
		return
	}
	if standaloneAnnotationRe.MatchString(line) {
		if v := declaredVersion(line); v != (macOSVersion{10, 0}) {
			s.pending = &v
		}
		return
	}
	s.pending = nil
}

// isDeclarationStart reports whether a line opens a method or property declaration.
func isDeclarationStart(line string) bool {
	return strings.HasPrefix(line, "- (") || strings.HasPrefix(line, "+ (") ||
		strings.HasPrefix(line, "-(") || strings.HasPrefix(line, "+(") ||
		strings.HasPrefix(line, "@property")
}

// joinDeclaration glues a declaration back together across line wraps, returning
// the joined text and the index of its final line.
func joinDeclaration(lines []string, start int) (string, int) {
	decl := strings.TrimSpace(lines[start])
	i := start
	for !strings.Contains(decl, ";") && i+1 < len(lines) {
		i++
		decl += " " + strings.TrimSpace(lines[i])
	}
	return decl, i
}

// declaredVersion extracts the macOS version from an availability macro, falling
// back to 10.0 when a declaration carries no annotation (meaning "always there").
func declaredVersion(decl string) macOSVersion {
	if m := availabilityMacroRe.FindStringSubmatch(decl); m != nil {
		if v, ok := parseMacOSVersion(m[1]); ok {
			return v
		}
	}
	if m := nsAvailabilityRe.FindStringSubmatch(decl); m != nil {
		if v, ok := parseMacOSVersion(m[1]); ok {
			return v
		}
	}
	return macOSVersion{10, 0}
}

// declaredVersionWith prefers a declaration's own annotation and falls back to one
// carried over from a preceding line.
func declaredVersionWith(decl string, pending *macOSVersion) macOSVersion {
	if v := declaredVersion(decl); v != (macOSVersion{10, 0}) {
		return v
	}
	if pending != nil {
		return *pending
	}
	return macOSVersion{10, 0}
}

// indexProperty records the accessor selectors a @property synthesizes.
func indexProperty(decl string, version macOSVersion, index *sdkAvailabilityIndex) {
	body := strings.TrimSpace(strings.TrimPrefix(decl, "@property"))
	attrs := ""
	if strings.HasPrefix(body, "(") {
		if end := strings.Index(body, ")"); end != -1 {
			attrs = body[:end+1]
			body = body[end+1:]
		}
	}
	m := propertyNameRe.FindStringSubmatch(body)
	if m == nil {
		return
	}
	name := m[1]

	getter := name
	if g := propertyGetterRe.FindStringSubmatch(attrs); g != nil {
		getter = g[1]
	}
	note(index.selectors, getter, version)

	if strings.Contains(attrs, "readonly") {
		return
	}
	setter := "set" + strings.ToUpper(name[:1]) + name[1:] + ":"
	if s := propertySetterRe.FindStringSubmatch(attrs); s != nil {
		setter = s[1]
	}
	note(index.selectors, setter, version)
}

// selectorFromDeclaration reconstructs the selector from a method declaration,
// for example "- (void)curveToPoint:(NSPoint)p controlPoint:(NSPoint)c;" becomes
// "curveToPoint:controlPoint:".
func selectorFromDeclaration(decl string) string {
	trimmed := decl
	for _, cut := range annotationCuts {
		if idx := strings.Index(trimmed, cut); idx != -1 {
			trimmed = trimmed[:idx]
		}
	}
	parts := selectorPartRe.FindAllStringSubmatch(trimmed, -1)
	if len(parts) == 0 {
		if m := zeroArgSelectorRe.FindStringSubmatch(trimmed); m != nil {
			return m[1]
		}
		return ""
	}
	var sb strings.Builder
	for _, p := range parts {
		sb.WriteString(p[1])
		sb.WriteString(":")
	}
	return sb.String()
}

// scanRustForNewerAPIs walks the desktop sources and reports every call site or
// class reference the SDK says needs a newer macOS than the floor.
func scanRustForNewerAPIs(srcDir string, index sdkAvailabilityIndex, floor macOSVersion) ([]availabilityFinding, int, error) {
	var findings []availabilityFinding
	scanned := 0

	err := filepath.Walk(srcDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info.IsDir() || !strings.HasSuffix(path, ".rs") {
			return nil
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return fmt.Errorf("failed to read %s: %w", path, err)
		}
		rel, relErr := filepath.Rel(srcDir, path)
		if relErr != nil {
			rel = path
		}

		for lineNum, line := range strings.Split(string(data), "\n") {
			if strings.HasPrefix(strings.TrimSpace(line), "//") {
				continue
			}
			lineFindings, lineScanned := scanLineForNewerAPIs(line, rel, lineNum+1, index, floor)
			findings = append(findings, lineFindings...)
			scanned += lineScanned
		}
		return nil
	})
	if err != nil {
		return nil, 0, err
	}
	return findings, scanned, nil
}

// scanLineForNewerAPIs reports the offending symbols on one line, plus how many
// call sites it recognized (so a clean run can say what it actually covered).
func scanLineForNewerAPIs(line, file string, lineNum int, index sdkAvailabilityIndex, floor macOSVersion) ([]availabilityFinding, int) {
	var findings []availabilityFinding
	scanned := 0

	for _, m := range objcCallRe.FindAllStringSubmatch(line, -1) {
		selector := objcSelectorFor(m[1])
		version, known := index.selectors[selector]
		if !known {
			continue
		}
		scanned++
		_, exempt := selectorsAssumedOld[selector]
		if version.newerThan(floor) && !exempt {
			findings = append(findings, availabilityFinding{
				file: file, line: lineNum, symbol: selector, version: version, kind: "selector",
			})
		}
	}

	for _, m := range objcClassRe.FindAllStringSubmatch(line, -1) {
		version, known := index.classes[m[1]]
		if !known {
			continue
		}
		scanned++
		_, gated := runtimeGatedClasses[m[1]]
		if version.newerThan(floor) && !gated {
			findings = append(findings, availabilityFinding{
				file: file, line: lineNum, symbol: m[1], version: version, kind: "class",
			})
		}
	}

	return findings, scanned
}

// objcSelectorFor turns an objc2 method name back into an ObjC selector.
func objcSelectorFor(name string) string {
	if !strings.Contains(name, "_") {
		return name
	}
	return strings.ReplaceAll(name, "_", ":") + ":"
}

// formatAvailabilityError lists every offending call site, one per symbol so a
// repeated selector doesn't drown the report.
func formatAvailabilityError(findings []availabilityFinding, floor macOSVersion) error {
	sort.Slice(findings, func(i, j int) bool {
		if findings[i].version != findings[j].version {
			return findings[j].version.newerThan(findings[i].version)
		}
		if findings[i].symbol != findings[j].symbol {
			return findings[i].symbol < findings[j].symbol
		}
		return findings[i].file < findings[j].file
	})

	var sb strings.Builder
	seen := map[string]bool{}
	unique := 0
	for _, f := range findings {
		if seen[f.symbol] {
			continue
		}
		seen[f.symbol] = true
		unique++
		sb.WriteString(fmt.Sprintf("  %s:%d %s %s needs macOS %s\n", f.file, f.line, f.kind, f.symbol, f.version))
	}

	return fmt.Errorf("%d %s newer than the macOS %s floor in Info.plist:\n%s\nEither gate the call at runtime, use an older equivalent, or raise LSMinimumSystemVersion",
		unique, Pluralize(unique, "API", "APIs"), floor,
		strings.TrimRight(sb.String(), "\n"))
}
