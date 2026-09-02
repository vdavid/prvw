package checks

import "testing"

func TestParseMacOSVersionAcceptsBothAnnotationSpellings(t *testing.T) {
	cases := map[string]macOSVersion{
		"14.0":   {14, 0},
		"10.15":  {10, 15},
		"10_15":  {10, 15},
		"26":     {26, 0},
		"13.0.1": {13, 0},
	}
	for input, want := range cases {
		got, ok := parseMacOSVersion(input)
		if !ok || got != want {
			t.Errorf("parseMacOSVersion(%q) = %v, %v; want %v, true", input, got, ok, want)
		}
	}
	if _, ok := parseMacOSVersion("sonoma"); ok {
		t.Error("parseMacOSVersion(\"sonoma\") should fail")
	}
}

func TestObjcSelectorForRebuildsTheSelector(t *testing.T) {
	cases := map[string]string{
		"curveToPoint_controlPoint":                "curveToPoint:controlPoint:",
		"curveToPoint_controlPoint1_controlPoint2": "curveToPoint:controlPoint1:controlPoint2:",
		"closePath": "closePath",
		"setImage":  "setImage",
		"generateBestRepresentationForRequest_completionHandler": "generateBestRepresentationForRequest:completionHandler:",
	}
	for input, want := range cases {
		if got := objcSelectorFor(input); got != want {
			t.Errorf("objcSelectorFor(%q) = %q; want %q", input, got, want)
		}
	}
}

func TestIndexHeaderReadsAvailabilityInEveryPosition(t *testing.T) {
	// Each construct here mirrors real SDK formatting that the parser has to
	// handle: a trailing macro on a wrapped method (how `curveToPoint:controlPoint:`
	// is written), a macro on the line above an `@interface`, a category that must
	// not reset its class back to "always available", and members inheriting their
	// class's floor (`cornerRadius` is unreachable before its class exists).
	header := `
/// A view that embeds its content view in a dynamic glass effect.
API_AVAILABLE(macos(26.0))
@interface NSGlassEffectView: NSView
@property (readonly) CGFloat cornerRadius;
@end

@interface NSBezierPath : NSObject
@property BOOL bordered API_AVAILABLE(macos(12.0));
- (void)closePath;
- (void)curveToPoint:(NSPoint)endPoint
        controlPoint:(NSPoint)controlPoint API_AVAILABLE(macos(14.0));
- (void)lineToPoint:(NSPoint)point;
+ (NSBezierPath *)bezierPathWithCGPath:(CGPathRef)cgPath API_AVAILABLE(macos(14.0));
@end

@interface NSGlassEffectView (LegacyHelpers)
- (void)someHelper;
@end

@interface NSOldThing : NSObject
- (void)ancientMethod NS_AVAILABLE_MAC(10_7);
@end
`
	index := sdkAvailabilityIndex{
		selectors: map[string]macOSVersion{},
		classes:   map[string]macOSVersion{},
	}
	indexHeader(header, &index)

	wantClasses := map[string]macOSVersion{
		"NSGlassEffectView": {26, 0}, // the category must not drag this to 10.0
		"NSBezierPath":      {10, 0},
		"NSOldThing":        {10, 0},
	}
	for name, want := range wantClasses {
		got, ok := index.classes[name]
		if !ok {
			t.Errorf("class %s not indexed", name)
			continue
		}
		if got != want {
			t.Errorf("class %s = macOS %v; want %v", name, got, want)
		}
	}

	wantSelectors := map[string]macOSVersion{
		"curveToPoint:controlPoint:": {14, 0},
		"bezierPathWithCGPath:":      {14, 0},
		"closePath":                  {10, 0},
		"lineToPoint:":               {10, 0},
		"cornerRadius":               {26, 0},
		"bordered":                   {12, 0},
		"setBordered:":               {12, 0},
		"ancientMethod":              {10, 7},
	}
	for sel, want := range wantSelectors {
		got, ok := index.selectors[sel]
		if !ok {
			t.Errorf("selector %s not indexed", sel)
			continue
		}
		if got != want {
			t.Errorf("selector %s = macOS %v; want %v", sel, got, want)
		}
	}

	// `cornerRadius` is readonly, so no setter should exist.
	if _, ok := index.selectors["setCornerRadius:"]; ok {
		t.Error("readonly property synthesized a setter")
	}
}

// TestRealSDKKnowsTheCurveToPointCrash anchors the check against the actual bug
// report that motivated it: prvw v0.15.1 aborted on macOS 13 because this
// selector is macOS 14+. If the parser ever stops seeing it, the check is blind.
func TestRealSDKKnowsTheCurveToPointCrash(t *testing.T) {
	sdk, err := macOSSDKPath()
	if err != nil {
		t.Skip("no macOS SDK available")
	}
	index, err := buildSDKAvailabilityIndex(sdk)
	if err != nil {
		t.Fatalf("failed to index SDK: %v", err)
	}

	v, ok := index.selectors["curveToPoint:controlPoint:"]
	if !ok {
		t.Fatal("curveToPoint:controlPoint: missing from the SDK index")
	}
	if v != (macOSVersion{14, 0}) {
		t.Errorf("curveToPoint:controlPoint: = macOS %v; want 14.0", v)
	}

	// The cubic form we replaced it with has always been there.
	if v, ok := index.selectors["curveToPoint:controlPoint1:controlPoint2:"]; !ok || v.newerThan(macOSVersion{10, 0}) {
		t.Errorf("curveToPoint:controlPoint1:controlPoint2: = macOS %v (present: %v); want 10.0", v, ok)
	}

	// NSSwitch is what stops us going below 10.15, so the report depends on it.
	if v, ok := index.classes["NSSwitch"]; !ok || v != (macOSVersion{10, 15}) {
		t.Errorf("NSSwitch = macOS %v (present: %v); want 10.15", v, ok)
	}
}

func TestFindingsReportIsDeterministicAndDeduplicated(t *testing.T) {
	findings := []availabilityFinding{
		{file: "b.rs", line: 2, symbol: "curveToPoint:controlPoint:", version: macOSVersion{14, 0}, kind: "selector"},
		{file: "a.rs", line: 9, symbol: "curveToPoint:controlPoint:", version: macOSVersion{14, 0}, kind: "selector"},
		{file: "c.rs", line: 1, symbol: "NSGlassEffectView", version: macOSVersion{26, 0}, kind: "class"},
	}
	err := formatAvailabilityError(findings, macOSVersion{13, 0})
	if err == nil {
		t.Fatal("expected an error")
	}
	msg := err.Error()
	// Two unique symbols, oldest-offender first.
	if got := countOccurrences(msg, "curveToPoint:controlPoint:"); got != 1 {
		t.Errorf("selector listed %d times; want 1 (deduplicated)", got)
	}
	if idxCurve, idxGlass := indexOf(msg, "curveToPoint"), indexOf(msg, "NSGlassEffectView"); idxCurve > idxGlass {
		t.Error("expected macOS 14.0 finding to sort before the macOS 26.0 one")
	}
}

func countOccurrences(haystack, needle string) int {
	count, offset := 0, 0
	for {
		i := indexOfFrom(haystack, needle, offset)
		if i < 0 {
			return count
		}
		count++
		offset = i + len(needle)
	}
}

func indexOf(haystack, needle string) int { return indexOfFrom(haystack, needle, 0) }

func indexOfFrom(haystack, needle string, offset int) int {
	if offset >= len(haystack) {
		return -1
	}
	for i := offset; i+len(needle) <= len(haystack); i++ {
		if haystack[i:i+len(needle)] == needle {
			return i
		}
	}
	return -1
}
