//! Build a natural-language summary of the current default image-handler state.
//!
//! Pure, side-effect-free. The caller passes in the already-queried handler data as
//! `&[FormatHandler]`. The function returns a single short sentence (or two, at most)
//! suitable for onboarding UI copy.
//!
//! Grouping rules:
//! - One sentence covers every format if they all share the same handler, all point
//!   at Prvw, or all have no default.
//! - Otherwise we summarize per group (standard image formats vs. camera RAW) and
//!   join the two with `, and `.
//! - Inside a group, if ≥⅔ of formats share one handler, that handler becomes the
//!   "most …" summary and we spell out the minority in a parenthetical. Otherwise
//!   we list every handler with its formats, separated by semicolons.
//!
//! "All Prvw" decision: we return a sentence, but the onboarding UI is expected to
//! hide the section entirely in that case. The sentence exists so callers that do
//! show it (for example, a Settings summary) still get something reasonable.
//!
//! The onboarding UI doesn't consume this module yet; it will once the onboarding
//! window gets its copy refresh. The tests are the spec and cover every shape of
//! output we care about. `#![allow(dead_code)]` suppresses clippy's dead-code lint
//! until the UI starts using it.

#![allow(dead_code)]

/// A single row of the "what currently opens this format" table.
#[derive(Clone, Debug)]
pub struct FormatHandler {
    /// Short format name, like `"JPEG"` or `"DNG"`.
    pub format_label: &'static str,
    pub group: FormatGroup,
    /// Display name of the current default handler, like `"Preview.app"`. `None`
    /// when no default is set.
    pub current_handler: Option<String>,
    /// True when the current handler is Prvw itself. Lets us print `"Prvw"` instead
    /// of `"Prvw.app"`.
    pub is_prvw: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FormatGroup {
    Standard,
    Raw,
}

/// Build the natural-language summary sentence.
#[must_use]
pub fn describe_defaults(handlers: &[FormatHandler]) -> String {
    if handlers.is_empty() {
        return "No image formats to report on right now.".to_string();
    }

    if let Some(sentence) = describe_uniform_state(handlers) {
        return sentence;
    }

    let standard: Vec<&FormatHandler> = handlers
        .iter()
        .filter(|h| h.group == FormatGroup::Standard)
        .collect();
    let raw: Vec<&FormatHandler> = handlers
        .iter()
        .filter(|h| h.group == FormatGroup::Raw)
        .collect();

    let standard_clause = describe_group(&standard, GroupLabel::Standard);
    let raw_clause = describe_group(&raw, GroupLabel::Raw);

    match (standard_clause, raw_clause) {
        (Some(s), Some(r)) => format!("{} {s}, and {r}.", lead_verb(&s)),
        (Some(s), None) => format!("{} {s}.", lead_verb(&s)),
        (None, Some(r)) => format!("{} {r}.", lead_verb(&r)),
        // Unreachable: handlers is non-empty, so at least one group clause is Some.
        (None, None) => "No image formats to report on right now.".to_string(),
    }
}

/// Pick the right leading verb for a clause. `"You use Preview.app for X"` reads
/// well; `"You use no default for X"` doesn't. When the clause starts with
/// `"no default"`, `"You have"` reads more naturally.
fn lead_verb(clause: &str) -> &'static str {
    if clause.starts_with("no default") {
        "You have"
    } else {
        "You use"
    }
}

/// Catch the whole-input cases: all on one handler, all on Prvw, or all with no
/// default. Returns `None` when the state is mixed and the caller needs per-group
/// logic.
fn describe_uniform_state(handlers: &[FormatHandler]) -> Option<String> {
    // A single format gets a tailored sentence (same shape, different phrasing,
    // because "all of these" would be odd for one item).
    if handlers.len() == 1 {
        return Some(describe_single(&handlers[0]));
    }

    if handlers.iter().all(|h| h.is_prvw) {
        return Some("Prvw is already set as the default for all of these.".to_string());
    }

    if handlers.iter().all(|h| h.current_handler.is_none()) {
        return Some("None of these formats have a default app right now.".to_string());
    }

    let first = handlers[0].current_handler.as_deref();
    let all_same = handlers
        .iter()
        .all(|h| h.current_handler.as_deref() == first && !h.is_prvw);
    if all_same && let Some(handler) = first {
        return Some(format!("You currently use {handler} for all of these."));
    }

    None
}

fn describe_single(h: &FormatHandler) -> String {
    if h.is_prvw {
        format!("Prvw is already set as the default for {}.", h.format_label)
    } else if let Some(handler) = &h.current_handler {
        format!("You currently use {handler} for {}.", h.format_label)
    } else {
        format!("{} has no default app right now.", h.format_label)
    }
}

#[derive(Copy, Clone)]
enum GroupLabel {
    Standard,
    Raw,
}

impl GroupLabel {
    fn most(self) -> &'static str {
        match self {
            Self::Standard => "most standard formats",
            Self::Raw => "most RAW formats",
        }
    }

    fn all(self) -> &'static str {
        match self {
            Self::Standard => "all standard formats",
            Self::Raw => "all RAW formats",
        }
    }
}

/// A bucket of formats that share one "slot": either a specific handler, or no
/// default at all. The `handler` field is `None` for the no-default bucket.
struct Bucket<'a> {
    /// `None` means "no default app"; `Some(name)` is the handler's display name
    /// already resolved to the right form (`"Prvw"` or `"Photoshop.app"`).
    handler: Option<String>,
    labels: Vec<&'a str>,
}

impl<'a> Bucket<'a> {
    /// `"Preview.app for JPEG and PNG"` or `"no default for BMP and TIFF"`.
    fn as_phrase(&self) -> String {
        let labels = join_labels(&self.labels);
        match &self.handler {
            Some(name) => format!("{name} for {labels}"),
            None => format!("no default for {labels}"),
        }
    }
}

fn describe_group(handlers: &[&FormatHandler], label: GroupLabel) -> Option<String> {
    if handlers.is_empty() {
        return None;
    }

    let buckets = bucket_by_handler(handlers);

    // Entire group shares one bucket → collapse to the group-level phrase.
    if buckets.len() == 1 {
        let only = &buckets[0];
        return Some(match &only.handler {
            Some(name) => format!("{name} for {}", label.all()),
            None => format!("no default for {}", label.all()),
        });
    }

    // Look for a dominant bucket (≥⅔ of the group). Ties pick the first found,
    // which is fine because a tie means no one bucket is ≥⅔.
    let total = handlers.len();
    let threshold = (total * 2).div_ceil(3); // ⌈2n/3⌉, the ≥⅔ cutoff.
    let dominant_idx = buckets
        .iter()
        .enumerate()
        .find(|(_, b)| b.labels.len() >= threshold)
        .map(|(i, _)| i);

    if let Some(idx) = dominant_idx {
        let dominant = &buckets[idx];
        let minority: Vec<&Bucket> = buckets
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != idx)
            .map(|(_, b)| b)
            .collect();
        let head = match &dominant.handler {
            Some(name) => format!("{name} for {}", label.most()),
            None => format!("no default for {}", label.most()),
        };
        let tail = join_minority(&minority);
        return Some(format!("{head} (and {tail})"));
    }

    // No dominant handler: list every bucket with semicolons between.
    let phrases: Vec<String> = buckets.iter().map(Bucket::as_phrase).collect();
    Some(phrases.join("; "))
}

/// Group formats by their current handler (or lack thereof), preserving the order
/// they first appear in the input. Stable order keeps the output deterministic
/// and predictable as callers reorder the source list.
fn bucket_by_handler<'a>(handlers: &[&'a FormatHandler]) -> Vec<Bucket<'a>> {
    let mut buckets: Vec<Bucket<'a>> = Vec::new();
    for h in handlers {
        let key = handler_display(h);
        match buckets.iter_mut().find(|b| b.handler == key) {
            Some(existing) => existing.labels.push(h.format_label),
            None => buckets.push(Bucket {
                handler: key,
                labels: vec![h.format_label],
            }),
        }
    }
    buckets
}

/// Resolve a `FormatHandler` to the name we display. `None` means "no default".
fn handler_display(h: &FormatHandler) -> Option<String> {
    if h.is_prvw {
        Some("Prvw".to_string())
    } else {
        h.current_handler.clone()
    }
}

/// Join format labels with an Oxford comma. Two items get `"A and B"`; three or
/// more get `"A, B, and C"`.
fn join_labels(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [only] => (*only).to_string(),
        [a, b] => format!("{a} and {b}"),
        [.., last] => {
            let head = labels[..labels.len() - 1].join(", ");
            format!("{head}, and {last}")
        }
    }
}

/// Join the minority buckets inside a parenthetical. The caller already put
/// the leading `"and "` ahead of this, so we only chain phrases here.
fn join_minority(buckets: &[&Bucket]) -> String {
    let parts: Vec<String> = buckets.iter().map(|b| b.as_phrase()).collect();
    match parts.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [.., last] => {
            let head = parts[..parts.len() - 1].join(", ");
            format!("{head}, and {last}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn std_fmt(label: &'static str, handler: Option<&str>) -> FormatHandler {
        FormatHandler {
            format_label: label,
            group: FormatGroup::Standard,
            current_handler: handler.map(String::from),
            is_prvw: false,
        }
    }

    fn raw_fmt(label: &'static str, handler: Option<&str>) -> FormatHandler {
        FormatHandler {
            format_label: label,
            group: FormatGroup::Raw,
            current_handler: handler.map(String::from),
            is_prvw: false,
        }
    }

    fn std_prvw(label: &'static str) -> FormatHandler {
        FormatHandler {
            format_label: label,
            group: FormatGroup::Standard,
            current_handler: Some("com.veszelovszki.prvw".into()),
            is_prvw: true,
        }
    }

    fn raw_prvw(label: &'static str) -> FormatHandler {
        FormatHandler {
            format_label: label,
            group: FormatGroup::Raw,
            current_handler: Some("com.veszelovszki.prvw".into()),
            is_prvw: true,
        }
    }

    /// The real 6 standard labels from `SUPPORTED_STANDARD_UTIS`.
    const STD_LABELS: &[&str] = &["JPEG", "PNG", "GIF", "WebP", "BMP", "TIFF"];

    /// The real 10 RAW labels from `SUPPORTED_RAW_UTIS`.
    const RAW_LABELS: &[&str] = &[
        "DNG", "CR2", "CR3", "NEF", "ARW", "ORF", "RAF", "RW2", "PEF", "SRW",
    ];

    fn all_standard_on(handler: Option<&str>) -> Vec<FormatHandler> {
        STD_LABELS.iter().map(|l| std_fmt(l, handler)).collect()
    }

    fn all_raw_on(handler: Option<&str>) -> Vec<FormatHandler> {
        RAW_LABELS.iter().map(|l| raw_fmt(l, handler)).collect()
    }

    fn full_list_on(handler: Option<&str>) -> Vec<FormatHandler> {
        let mut v = all_standard_on(handler);
        v.extend(all_raw_on(handler));
        v
    }

    #[test]
    fn empty_input_returns_safe_sentence() {
        assert_eq!(
            describe_defaults(&[]),
            "No image formats to report on right now."
        );
    }

    #[test]
    fn all_sixteen_on_same_handler() {
        let handlers = full_list_on(Some("Preview.app"));
        assert_eq!(
            describe_defaults(&handlers),
            "You currently use Preview.app for all of these."
        );
    }

    #[test]
    fn all_sixteen_on_prvw() {
        let mut handlers: Vec<FormatHandler> = STD_LABELS.iter().map(|l| std_prvw(l)).collect();
        handlers.extend(RAW_LABELS.iter().map(|l| raw_prvw(l)));
        assert_eq!(
            describe_defaults(&handlers),
            "Prvw is already set as the default for all of these."
        );
    }

    #[test]
    fn all_sixteen_with_no_default() {
        let handlers = full_list_on(None);
        assert_eq!(
            describe_defaults(&handlers),
            "None of these formats have a default app right now."
        );
    }

    #[test]
    fn standard_on_preview_raw_has_no_default() {
        let mut handlers = all_standard_on(Some("Preview.app"));
        handlers.extend(all_raw_on(None));
        assert_eq!(
            describe_defaults(&handlers),
            "You use Preview.app for all standard formats, and no default for all RAW formats."
        );
    }

    #[test]
    fn raw_on_one_handler_standard_mixed() {
        // 4/6 Preview, 2/6 Photoshop → dominant. 10/10 Photos.
        let mut handlers = vec![
            std_fmt("JPEG", Some("Preview.app")),
            std_fmt("PNG", Some("Photoshop.app")),
            std_fmt("GIF", Some("Preview.app")),
            std_fmt("WebP", Some("Preview.app")),
            std_fmt("BMP", Some("Preview.app")),
            std_fmt("TIFF", Some("Photoshop.app")),
        ];
        handlers.extend(all_raw_on(Some("Photos.app")));
        assert_eq!(
            describe_defaults(&handlers),
            "You use Preview.app for most standard formats (and Photoshop.app for PNG and TIFF), and Photos.app for all RAW formats."
        );
    }

    #[test]
    fn five_of_six_standard_on_preview_one_outlier() {
        let handlers = vec![
            std_fmt("JPEG", Some("Preview.app")),
            std_fmt("PNG", Some("Photoshop.app")),
            std_fmt("GIF", Some("Preview.app")),
            std_fmt("WebP", Some("Preview.app")),
            std_fmt("BMP", Some("Preview.app")),
            std_fmt("TIFF", Some("Preview.app")),
        ];
        assert_eq!(
            describe_defaults(&handlers),
            "You use Preview.app for most standard formats (and Photoshop.app for PNG)."
        );
    }

    #[test]
    fn four_of_six_standard_on_preview_two_outliers_on_different_apps() {
        // 4/6 = 66.6% on Preview. Threshold is ⌈⅔·6⌉ = 4, so Preview is dominant.
        let handlers = vec![
            std_fmt("JPEG", Some("Preview.app")),
            std_fmt("PNG", Some("Photoshop.app")),
            std_fmt("GIF", Some("Preview.app")),
            std_fmt("WebP", Some("Preview.app")),
            std_fmt("BMP", Some("Preview.app")),
            std_fmt("TIFF", None),
        ];
        assert_eq!(
            describe_defaults(&handlers),
            "You use Preview.app for most standard formats (and Photoshop.app for PNG, and no default for TIFF)."
        );
    }

    #[test]
    fn three_of_six_standard_half_and_half_lists_by_handler() {
        // 3 on Preview, 3 with no default → no dominant bucket.
        let handlers = vec![
            std_fmt("JPEG", Some("Preview.app")),
            std_fmt("PNG", Some("Preview.app")),
            std_fmt("GIF", Some("Preview.app")),
            std_fmt("WebP", None),
            std_fmt("BMP", None),
            std_fmt("TIFF", None),
        ];
        assert_eq!(
            describe_defaults(&handlers),
            "You use Preview.app for JPEG, PNG, and GIF; no default for WebP, BMP, and TIFF."
        );
    }

    #[test]
    fn nine_of_ten_raw_on_preview_one_outlier_no_default() {
        let mut handlers = vec![raw_fmt("DNG", None)];
        handlers.extend(
            [
                "CR2", "CR3", "NEF", "ARW", "ORF", "RAF", "RW2", "PEF", "SRW",
            ]
            .iter()
            .map(|l| raw_fmt(l, Some("Preview.app"))),
        );
        assert_eq!(
            describe_defaults(&handlers),
            "You use Preview.app for most RAW formats (and no default for DNG)."
        );
    }

    #[test]
    fn mixed_standard_with_outlier_plus_dominant_raw() {
        // Matches the spec's "You use Preview.app for most standard formats
        // (and Photoshop for PNG), and Photos for most RAW formats
        // (except DNG, which has no default)" example, adapted to our phrasing.
        let mut handlers = vec![
            std_fmt("JPEG", Some("Preview.app")),
            std_fmt("PNG", Some("Photoshop.app")),
            std_fmt("GIF", Some("Preview.app")),
            std_fmt("WebP", Some("Preview.app")),
            std_fmt("BMP", Some("Preview.app")),
            std_fmt("TIFF", Some("Preview.app")),
        ];
        handlers.push(raw_fmt("DNG", None));
        for label in &[
            "CR2", "CR3", "NEF", "ARW", "ORF", "RAF", "RW2", "PEF", "SRW",
        ] {
            handlers.push(raw_fmt(label, Some("Photos.app")));
        }
        assert_eq!(
            describe_defaults(&handlers),
            "You use Preview.app for most standard formats (and Photoshop.app for PNG), \
             and Photos.app for most RAW formats (and no default for DNG)."
        );
    }

    #[test]
    fn single_format_with_handler() {
        assert_eq!(
            describe_defaults(&[std_fmt("JPEG", Some("Preview.app"))]),
            "You currently use Preview.app for JPEG."
        );
    }

    #[test]
    fn single_format_with_no_default() {
        assert_eq!(
            describe_defaults(&[std_fmt("JPEG", None)]),
            "JPEG has no default app right now."
        );
    }

    #[test]
    fn single_format_on_prvw() {
        let h = FormatHandler {
            format_label: "JPEG",
            group: FormatGroup::Standard,
            current_handler: Some("com.veszelovszki.prvw".into()),
            is_prvw: true,
        };
        assert_eq!(
            describe_defaults(&[h]),
            "Prvw is already set as the default for JPEG."
        );
    }

    #[test]
    fn unicode_and_special_chars_in_handler_name() {
        let handlers = all_standard_on(Some("Photo-Viewer™.app"));
        assert_eq!(
            describe_defaults(&handlers),
            "You currently use Photo-Viewer™.app for all of these."
        );
    }

    #[test]
    fn prvw_shown_without_app_suffix_in_mixed_state() {
        // Prvw owns standard; Preview owns RAW.
        let mut handlers: Vec<FormatHandler> = STD_LABELS.iter().map(|l| std_prvw(l)).collect();
        handlers.extend(all_raw_on(Some("Preview.app")));
        assert_eq!(
            describe_defaults(&handlers),
            "You use Prvw for all standard formats, and Preview.app for all RAW formats."
        );
    }

    #[test]
    fn only_standard_group_present() {
        let handlers = all_standard_on(Some("Preview.app"));
        assert_eq!(
            describe_defaults(&handlers),
            "You currently use Preview.app for all of these."
        );
    }

    #[test]
    fn only_raw_group_present_with_mixed_state() {
        // 7/10 Preview, 3/10 Photos → 7 ≥ ⌈⅔·10⌉ = 7, so Preview is dominant.
        let mut handlers = Vec::new();
        for label in &["DNG", "CR2", "CR3", "NEF", "ARW", "ORF", "RAF"] {
            handlers.push(raw_fmt(label, Some("Preview.app")));
        }
        for label in &["RW2", "PEF", "SRW"] {
            handlers.push(raw_fmt(label, Some("Photos.app")));
        }
        assert_eq!(
            describe_defaults(&handlers),
            "You use Preview.app for most RAW formats (and Photos.app for RW2, PEF, and SRW)."
        );
    }

    #[test]
    fn no_default_can_be_the_dominant_bucket() {
        // 5/6 standard with no default, 1 on Preview → "no default" is dominant.
        let handlers = vec![
            std_fmt("JPEG", None),
            std_fmt("PNG", Some("Preview.app")),
            std_fmt("GIF", None),
            std_fmt("WebP", None),
            std_fmt("BMP", None),
            std_fmt("TIFF", None),
        ];
        assert_eq!(
            describe_defaults(&handlers),
            "You have no default for most standard formats (and Preview.app for PNG)."
        );
    }

    #[test]
    fn standard_no_default_raw_on_handler_leads_with_have() {
        let mut handlers = all_standard_on(None);
        handlers.extend(all_raw_on(Some("Photos.app")));
        assert_eq!(
            describe_defaults(&handlers),
            "You have no default for all standard formats, and Photos.app for all RAW formats."
        );
    }

    #[test]
    fn prvw_mixed_with_preview_in_standard_group() {
        // 4 Prvw, 2 Preview in standard. Empty raw group.
        let handlers = vec![
            std_prvw("JPEG"),
            std_prvw("PNG"),
            std_prvw("GIF"),
            std_prvw("WebP"),
            std_fmt("BMP", Some("Preview.app")),
            std_fmt("TIFF", Some("Preview.app")),
        ];
        assert_eq!(
            describe_defaults(&handlers),
            "You use Prvw for most standard formats (and Preview.app for BMP and TIFF)."
        );
    }

    #[test]
    fn two_format_group_dominant_requires_both_same() {
        // With n=2, ⌈⅔·2⌉ = 2, so "most" only applies if both share a bucket,
        // and in that case we collapse to the group-level "all …" phrase.
        let handlers = vec![
            std_fmt("JPEG", Some("Preview.app")),
            std_fmt("PNG", Some("Photoshop.app")),
        ];
        // Only the standard group is present, so the output has no raw clause.
        assert_eq!(
            describe_defaults(&handlers),
            "You use Preview.app for JPEG; Photoshop.app for PNG."
        );
    }
}
