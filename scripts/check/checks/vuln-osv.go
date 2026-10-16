package checks

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

// cargo-audit's JSON gives a fix only as a requirement (`>=0.23.46`, or `^0.22.5, >=0.23.46`),
// which can't be dated. So `cargo-audit` looks each advisory up on OSV by ID, takes the fix the
// record names for the version we use, and dates that version on crates.io. Ported from Cmdr's
// `osv-scanner.go`, which does the same for its `cargo-deny` lane.

type osvVulnerability struct {
	ID        string    `json:"id"`
	Published time.Time `json:"published"`
	Summary   string    `json:"summary"`
	Affected  []struct {
		Package struct {
			Ecosystem string `json:"ecosystem"`
			Name      string `json:"name"`
		} `json:"package"`
		Ranges []osvRange `json:"ranges"`
	} `json:"affected"`
}

type osvEvent struct {
	Introduced   string `json:"introduced"`
	Fixed        string `json:"fixed"`
	LastAffected string `json:"last_affected"`
}

type osvRange struct {
	Type   string     `json:"type"`
	Events []osvEvent `json:"events"`
}

// osvFixFor fills in a's fix and publish date from its OSV record: the lowest fix any affected
// range names for our version of our package. a keeps its own summary.
func osvFixFor(a vulnAdvisory, v osvVulnerability) vulnAdvisory {
	a.FixedVersion, a.Published = "", v.Published
	for _, aff := range v.Affected {
		if aff.Package.Ecosystem != a.Ecosystem || !strings.EqualFold(aff.Package.Name, a.Module) {
			continue
		}
		if fixed := osvFixedVersion(aff.Ranges, a.Version); fixed != "" &&
			(a.FixedVersion == "" || compareLooseVersions(fixed, a.FixedVersion) < 0) {
			a.FixedVersion = fixed
		}
	}
	return a
}

// osvFixedVersion is the fix for the affected range `version` falls in, or "" when no range
// holds it or that range has no fix. GIT ranges name commits, so they're skipped.
func osvFixedVersion(ranges []osvRange, version string) string {
	for _, r := range ranges {
		if r.Type != "SEMVER" && r.Type != "ECOSYSTEM" {
			continue
		}
		introduced := ""
		for _, e := range r.Events {
			switch {
			case e.Introduced != "":
				introduced = e.Introduced
			case e.Fixed != "" && introduced != "":
				if compareLooseVersions(introduced, version) <= 0 && compareLooseVersions(version, e.Fixed) < 0 {
					return e.Fixed
				}
				introduced = ""
			}
		}
	}
	return ""
}

// compareLooseVersions orders dotted versions the way crates spell them: a leading `v` and
// `+build` metadata are ignored, missing parts count as zero, and a `-prerelease` sorts before
// its release. Not a full semver implementation; it only has to place a version against an
// advisory's range bounds.
func compareLooseVersions(a, b string) int {
	coreA, preA := splitLooseVersion(a)
	coreB, preB := splitLooseVersion(b)
	for i := 0; i < len(coreA) || i < len(coreB); i++ {
		if c := comparePart(partAt(coreA, i), partAt(coreB, i)); c != 0 {
			return c
		}
	}
	switch {
	case preA == preB:
		return 0
	case preA == "":
		return 1
	case preB == "":
		return -1
	case preA < preB:
		return -1
	default:
		return 1
	}
}

func splitLooseVersion(v string) (core []string, pre string) {
	v = strings.TrimPrefix(v, "v")
	if i := strings.IndexByte(v, '+'); i >= 0 {
		v = v[:i]
	}
	if i := strings.IndexByte(v, '-'); i >= 0 {
		v, pre = v[:i], v[i+1:]
	}
	return strings.Split(v, "."), pre
}

func partAt(parts []string, i int) string {
	if i < len(parts) {
		return parts[i]
	}
	return "0"
}

func comparePart(a, b string) int {
	na, errA := strconv.Atoi(a)
	nb, errB := strconv.Atoi(b)
	if errA == nil && errB == nil {
		switch {
		case na < nb:
			return -1
		case na > nb:
			return 1
		}
		return 0
	}
	return strings.Compare(a, b)
}

// crateReleaseTimes looks advisories up on OSV and dates crate versions on crates.io, once per
// crate@version per run.
type crateReleaseTimes struct {
	client *http.Client
	cache  map[string]goProxyAnswer
}

func newCrateReleaseTimes() *crateReleaseTimes {
	return &crateReleaseTimes{client: &http.Client{Timeout: 15 * time.Second}, cache: map[string]goProxyAnswer{}}
}

// osvFix looks a's ID up in the OSV database and fills in its fix (`osvFixFor`). A record it
// can't fetch leaves a without one, which fails it: waiting is only for a fix that provably exists.
func (r *crateReleaseTimes) osvFix(a vulnAdvisory) vulnAdvisory {
	var v osvVulnerability
	if !r.getJSON("https://api.osv.dev/v1/vulns/"+url.PathEscape(a.ID), &v) || v.ID != a.ID {
		return a
	}
	return osvFixFor(a, v)
}

// fixReleased dates an advisory's fixed version by its crates.io publish time.
func (r *crateReleaseTimes) fixReleased(a vulnAdvisory) (time.Time, bool) {
	key := a.Module + "@" + a.FixedVersion
	if answer, hit := r.cache[key]; hit {
		return answer.at, answer.ok
	}
	var body struct {
		Version struct {
			CreatedAt time.Time `json:"created_at"`
		} `json:"version"`
	}
	ok := r.getJSON(fmt.Sprintf("https://crates.io/api/v1/crates/%s/%s", url.PathEscape(a.Module), url.PathEscape(a.FixedVersion)), &body)
	answer := goProxyAnswer{at: body.Version.CreatedAt, ok: ok && !body.Version.CreatedAt.IsZero()}
	r.cache[key] = answer
	return answer.at, answer.ok
}

func (r *crateReleaseTimes) getJSON(target string, into any) bool {
	req, err := http.NewRequest(http.MethodGet, target, nil)
	if err != nil {
		return false
	}
	// crates.io refuses requests without a User-Agent that names who's asking.
	req.Header.Set("User-Agent", "prvw-check-runner (https://github.com/vdavid/prvw)")
	resp, err := r.client.Do(req)
	if err != nil || resp == nil {
		return false
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return false
	}
	return json.NewDecoder(resp.Body).Decode(into) == nil
}
