//! What the Windows settings dialog holds, as data a Mac can test.
//!
//! Every tab, every row, every description, every trackbar range, and the rule for turning a
//! control's new value into an [`AppCommand`] lives here, in plain Rust with no Win32 in sight.
//! `settings::windows::dialog` is then a reader: it walks [`Tab::ALL`], asks [`page`] what a tab
//! holds, and creates one control per row. That split is deliberate. Nothing in this project has
//! ever run on Windows, so the half that decides *what* the dialog says is the half worth
//! proving, and it's provable from any host.
//!
//! It follows `src/scroll.rs` and `src/paths.rs`: per-platform behaviour held as data, so a
//! macOS `cargo test` checks what a Windows user will see.
//!
//! ## The registry is the spine
//!
//! A row names a [`SettingKey`] and takes its title from [`SettingKey::label`], the same string
//! the macOS row wears. So the label can't drift between the two builds, and a row can't exist
//! without a key. What stays per-platform is `description`: the macOS copy talks about MacBook
//! screens and ⌘, and this one talks about monitors and Ctrl.
//!
//! ## Immediate apply, one path in
//!
//! There's no OK/Cancel/Apply. A click or a drag runs [`apply`], which folds the new value into
//! a copy of `Settings` and hands back the [`AppCommand`] that carries it to the app, exactly
//! as the macOS window does. `Settings` stays the one model; this is a second view of it.

use crate::commands::AppCommand;
use crate::decoding::{
    BASELINE_EXPOSURE_OFFSET_RANGE, CLARITY_AMOUNT_RANGE, CLARITY_RADIUS_RANGE, HDR_GAIN_RANGE,
    MIDTONE_ANCHOR_RANGE, RawPipelineFlags, SATURATION_BOOST_RANGE, SHARPEN_AMOUNT_RANGE,
};
use crate::parity::setting_keys::{Panel, SettingKey};
use crate::settings::Settings;
use crate::slideshow::{MAX_SECONDS, MIN_SECONDS, clamp_seconds};

/// One tab across the top of the dialog, in the order they appear.
///
/// Same six groupings as the macOS sidebar, in the same order, because
/// [`Panel`] is the product's shared answer to "where does this setting live". The widget is
/// where the two platforms part company: a `SysTabControl32` here, a sidebar there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    General,
    Zoom,
    Color,
    Raw,
    Slideshow,
    FileAssociations,
}

impl Tab {
    /// Every tab, left to right.
    pub const ALL: &'static [Tab] = &[
        Tab::General,
        Tab::Zoom,
        Tab::Color,
        Tab::Raw,
        Tab::Slideshow,
        Tab::FileAssociations,
    ];

    /// The settings panel this tab shows.
    pub const fn panel(self) -> Panel {
        match self {
            Tab::General => Panel::General,
            Tab::Zoom => Panel::Zoom,
            Tab::Color => Panel::Color,
            Tab::Raw => Panel::Raw,
            Tab::Slideshow => Panel::Slideshow,
            Tab::FileAssociations => Panel::FileAssociations,
        }
    }

    /// The tab's own caption, which is the panel's name.
    pub const fn title(self) -> &'static str {
        self.panel().name()
    }

    /// Which tab a QA `ShowSettingsSection` request names. The spellings match the macOS
    /// window's, so one E2E test drives both.
    pub fn from_section_name(section: &str) -> Option<Tab> {
        match section.to_lowercase().as_str() {
            "general" => Some(Tab::General),
            "zoom" => Some(Tab::Zoom),
            "color" => Some(Tab::Color),
            "raw" => Some(Tab::Raw),
            "slideshow" => Some(Tab::Slideshow),
            "file associations" | "file_associations" | "fileassociations" => {
                Some(Tab::FileAssociations)
            }
            _ => None,
        }
    }
}

/// How a trackbar's integer position maps onto the setting's own range.
///
/// `msctls_trackbar32` is an integer control, so every float setting needs a step count and a
/// rounding rule. [`Scale::position`] and [`Scale::value`] are inverses within one step, which
/// is what stops a drag from drifting the stored value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    pub min: f32,
    pub max: f32,
    /// Positions on the bar, so the range is `0..=steps`.
    pub steps: i32,
    pub format: ValueFormat,
}

/// How the read-only static beside a trackbar renders the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueFormat {
    /// Most knobs read right with two decimals.
    TwoDecimal,
    /// The clarity radius is 2 to 50 pixels, and reads better as a whole number.
    IntegerPx,
    /// Whole seconds, for the slideshow's time per image.
    Seconds,
}

impl ValueFormat {
    /// The text beside the bar.
    pub fn render(self, value: f32) -> String {
        match self {
            ValueFormat::TwoDecimal => format!("{value:.2}"),
            ValueFormat::IntegerPx => format!("{} px", value.round() as i32),
            ValueFormat::Seconds => format!("{}s", value.round() as i32),
        }
    }
}

impl Scale {
    const fn new(range: (f32, f32), steps: i32, format: ValueFormat) -> Self {
        Self {
            min: range.0,
            max: range.1,
            steps,
            format,
        }
    }

    /// Where `value` sits on the bar, clamped to the track.
    pub fn position(&self, value: f32) -> i32 {
        let span = self.max - self.min;
        if span <= 0.0 {
            return 0;
        }
        let fraction = ((value - self.min) / span).clamp(0.0, 1.0);
        (fraction * self.steps as f32).round() as i32
    }

    /// What position `position` means, clamped to the setting's range.
    pub fn value(&self, position: i32) -> f32 {
        let clamped = position.clamp(0, self.steps);
        let fraction = clamped as f32 / self.steps as f32;
        self.min + fraction * (self.max - self.min)
    }

    /// The text for the value static at this position.
    pub fn render(&self, value: f32) -> String {
        self.format.render(value)
    }
}

/// The control a row puts on screen. `SettingKey::control` names the job; this names the
/// Win32 widget, because two `Toggle`s can want different treatment here and only this side
/// knows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RowKind {
    /// `BUTTON` with `BS_AUTOCHECKBOX`. comctl32 has no switch, and a checkbox is what
    /// "native" means on Windows.
    Checkbox,
    /// `msctls_trackbar32` plus a right-aligned read-only static showing the value.
    Trackbar(Scale),
    /// A read-only edit control with Browse… and Clear beside it.
    Folder,
    /// The file-types surface: the extension list and the two buttons. Windows owns the
    /// default-handler choice, so this row can't be a set of toggles (see [`Page::file_types`]).
    FileTypes,
}

/// One line of a page.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub key: SettingKey,
    pub kind: RowKind,
    /// The grey line under the control. Windows copy, not a translation of the macOS copy.
    pub description: &'static str,
    /// True for a knob that belongs to the toggle above it, which gets it indented under it
    /// the way the macOS panel nests them.
    pub indented: bool,
}

impl Row {
    const fn check(key: SettingKey, description: &'static str) -> Self {
        Self {
            key,
            kind: RowKind::Checkbox,
            description,
            indented: false,
        }
    }

    const fn bar(
        key: SettingKey,
        description: &'static str,
        range: (f32, f32),
        format: ValueFormat,
    ) -> Self {
        Self {
            key,
            kind: RowKind::Trackbar(Scale::new(range, TRACKBAR_STEPS, format)),
            description,
            indented: true,
        }
    }

    /// The row's title, which is the registry's label. Never a string of this module's own:
    /// that's what keeps the two platforms calling a setting one name.
    pub const fn label(&self) -> &'static str {
        self.key.label()
    }
}

/// How many stops a trackbar has between its ends.
///
/// One number for every float knob, so no setting is secretly coarser than its neighbour. 100
/// is fine enough that a drag feels continuous and coarse enough that the arrow keys (one step
/// each) are usable.
const TRACKBAR_STEPS: i32 = 100;

/// A titled box of rows, drawn as a `BS_GROUPBOX`. `None` means the rows sit loose at the top
/// of the page, which is what the four small tabs want.
#[derive(Clone, Copy, Debug)]
pub struct Group {
    pub title: Option<&'static str>,
    pub rows: &'static [Row],
}

/// Everything one tab shows.
#[derive(Clone, Copy, Debug)]
pub struct Page {
    pub groups: &'static [Group],
    /// True for the RAW page, whose content is taller than the dialog and scrolls.
    pub scrolls: bool,
    /// True for the RAW page, which ends with the same "Reset to defaults" button macOS has.
    pub reset_button: bool,
    /// True for the File associations page, whose surface is the extension list and two
    /// buttons rather than a stack of rows.
    pub file_types: bool,
}

/// What `tab` holds.
pub const fn page(tab: Tab) -> Page {
    match tab {
        Tab::General => Page {
            groups: GENERAL,
            scrolls: false,
            reset_button: false,
            file_types: false,
        },
        Tab::Zoom => Page {
            groups: ZOOM,
            scrolls: false,
            reset_button: false,
            file_types: false,
        },
        Tab::Color => Page {
            groups: COLOR,
            scrolls: false,
            reset_button: false,
            file_types: false,
        },
        Tab::Raw => Page {
            groups: RAW,
            scrolls: true,
            reset_button: true,
            file_types: false,
        },
        Tab::Slideshow => Page {
            groups: SLIDESHOW,
            scrolls: false,
            reset_button: false,
            file_types: false,
        },
        Tab::FileAssociations => Page {
            groups: FILE_ASSOCIATIONS,
            scrolls: false,
            reset_button: false,
            file_types: true,
        },
    }
}

impl Page {
    /// Every row on the page, in the order they're stacked.
    pub fn rows(self) -> impl Iterator<Item = &'static Row> {
        self.groups.iter().flat_map(|group| group.rows.iter())
    }
}

/// Every row of every page, top to bottom, left tab to right. The dialog walks a page at a
/// time; this is what the tests sweep with.
#[cfg(test)]
pub fn all_rows() -> impl Iterator<Item = (Tab, &'static Row)> {
    Tab::ALL
        .iter()
        .flat_map(|tab| page(*tab).rows().map(move |row| (*tab, row)))
}

/// The row for `key`, or `None` where the Windows dialog has no row for it.
#[cfg(test)]
pub fn row_for(key: SettingKey) -> Option<&'static Row> {
    all_rows()
        .find(|(_, row)| row.key == key)
        .map(|(_, row)| row)
}

/// The dialog's buttons.
///
/// Named here rather than written into `dialog` so every user-visible string in the dialog
/// lives in one module. That's what lets [`user_visible_strings`] be exhaustive, and what lets
/// a Mac read the copy the Windows build will show.
pub mod button {
    pub const CLOSE: &str = "Close";
    pub const BROWSE: &str = "Browse\u{2026}";
    pub const CLEAR: &str = "Clear";
    pub const RESET: &str = "Reset to defaults";
    /// Writes the ProgID and the `OpenWithProgids` entries. It can't make Prvw the default,
    /// which is what the page's copy says and what `super::file_types` explains.
    pub const REGISTER_FILE_TYPES: &str = "Register Prvw's file types";
    pub const OPEN_DEFAULT_APPS: &str = "Open Windows default apps settings";

    /// All of them, for the copy sweep. `dialog` names the one it's creating.
    #[cfg(test)]
    pub const ALL: &[&str] = &[
        CLOSE,
        BROWSE,
        CLEAR,
        RESET,
        REGISTER_FILE_TYPES,
        OPEN_DEFAULT_APPS,
    ];
}

/// Everything the dialog labels something with: tab captions, group-box titles, row titles, and
/// buttons. Short strings, and the ones `docs/style-guide.md`'s sentence-case rule is about.
#[cfg(test)]
pub fn user_visible_titles() -> Vec<&'static str> {
    let mut titles: Vec<&'static str> = Vec::new();
    for tab in Tab::ALL {
        titles.push(tab.title());
        for group in page(*tab).groups {
            titles.extend(group.title);
            titles.extend(group.rows.iter().map(Row::label));
        }
    }
    titles.extend(button::ALL);
    titles
}

/// Every string the dialog puts on screen, so a test can hold all of them to one standard.
///
/// The titles above plus every description and both scroll-to-zoom lines. `about::content` does
/// the same for the About box; this dialog has thirty times as many strings, so the sweep
/// matters more here.
#[cfg(test)]
pub fn user_visible_strings() -> Vec<&'static str> {
    let mut strings = user_visible_titles();
    for tab in Tab::ALL {
        strings.extend(page(*tab).rows().map(|row| row.description));
    }
    strings.extend([SCROLL_TO_ZOOM_ON, SCROLL_TO_ZOOM_OFF]);
    strings
}

// ── The pages ────────────────────────────────────────────────────────────────

const GENERAL: &[Group] = &[Group {
    title: None,
    rows: &[
        Row::check(
            SettingKey::AutoUpdate,
            "Check for updates when Prvw starts.",
        ),
        Row::check(SettingKey::ScrollToZoom, SCROLL_TO_ZOOM_OFF),
        Row::check(
            SettingKey::PreloadNeighbors,
            "Decode the next and previous images in the background, so navigation is instant. \
             Turn it off to time a single cold decode.",
        ),
    ],
}];

/// The scroll-to-zoom description, which says something different depending on the setting.
/// The dialog swaps the static's text on every click, the way the macOS window does.
pub const SCROLL_TO_ZOOM_ON: &str = "The wheel zooms in and out instead of switching images.";
/// The other half of [`SCROLL_TO_ZOOM_ON`].
pub const SCROLL_TO_ZOOM_OFF: &str =
    "The wheel switches images. Ctrl+plus and Ctrl+minus still zoom.";

/// Which of the two lines belongs under the scroll-to-zoom checkbox right now.
pub const fn scroll_to_zoom_description(on: bool) -> &'static str {
    if on {
        SCROLL_TO_ZOOM_ON
    } else {
        SCROLL_TO_ZOOM_OFF
    }
}

const ZOOM: &[Group] = &[Group {
    title: None,
    rows: &[
        Row::check(
            SettingKey::AutoFitWindow,
            "Resize the window to match each image.",
        ),
        // Deliberately not greyed out by "Auto-fit window", the same as on macOS: auto-fit is
        // inert in fullscreen, where enlarge still governs.
        Row::check(
            SettingKey::EnlargeSmallImages,
            "Scale up images smaller than the window. Off by default, so small pictures stay \
             sharp.",
        ),
    ],
}];

const COLOR: &[Group] = &[Group {
    title: None,
    rows: &[
        Row::check(
            SettingKey::IccColorManagement,
            "Corrects the colors of images that carry an embedded profile, which is most \
             photos from a real camera. Without it, shots in Adobe RGB or ProPhoto look washed \
             out.",
        ),
        Row::check(
            SettingKey::ColorMatchDisplay,
            "Adapts colors to the profile Windows holds for your monitor instead of assuming a \
             standard sRGB screen. It matters most on wide-gamut and HDR panels. If Windows 11 \
             is running Auto Color Management on this display, it's already color-managing the \
             desktop, so leaving this off avoids converting twice.",
        ),
        Row::check(
            SettingKey::RelativeColorimetric,
            "Changes what happens to colors your monitor can't show. By default Prvw eases \
             every color into range (perceptual). With this on, the colors your monitor can \
             show stay exact and the rest are clipped.",
        ),
    ],
}];

const RAW: &[Group] = &[
    Group {
        title: Some("Sensor corrections (DNG only)"),
        rows: &[
            Row::check(
                SettingKey::RawDngOpcodeList1,
                "Pre-linearization gain maps and bad-pixel fixes (DNG only).",
            ),
            Row::check(
                SettingKey::RawDngOpcodeList2,
                "CFA-level gain maps and bad-pixel fixes (DNG only, iPhone ProRAW).",
            ),
            Row::check(
                SettingKey::RawDngOpcodeList3,
                "Post-color lens distortion correction (DNG only).",
            ),
        ],
    },
    Group {
        title: Some("Color"),
        rows: &[
            Row::check(
                SettingKey::RawBaselineExposure,
                "Apply the camera's intended baseline exposure (or a neutral default) plus the \
                 offset below.",
            ),
            Row::bar(
                SettingKey::RawBaselineExposureOffset,
                "Your own offset in EV stops on top of the camera or default baseline.",
                BASELINE_EXPOSURE_OFFSET_RANGE,
                ValueFormat::TwoDecimal,
            ),
            Row::check(
                SettingKey::RawDcpHueSatMap,
                "Per-camera color calibration table from the profile.",
            ),
            Row::check(
                SettingKey::RawDcpLookTable,
                "Adobe \u{201c}Look\u{201d} refinement applied after HueSatMap.",
            ),
            Row::check(
                SettingKey::RawSaturationBoost,
                "Mild global chroma lift in linear Rec.2020.",
            ),
            Row::bar(
                SettingKey::RawSaturationAmount,
                "Chroma lift strength in linear Rec.2020 (post-tone, pre-ICC).",
                SATURATION_BOOST_RANGE,
                ValueFormat::TwoDecimal,
            ),
        ],
    },
    Group {
        title: Some("Tone"),
        rows: &[
            Row::check(
                SettingKey::RawHighlightRecovery,
                "Desaturate near-clip pixels toward their own luminance.",
            ),
            Row::check(
                SettingKey::RawDefaultToneCurve,
                "Prvw's filmic S-curve: shadow lift and highlight shoulder.",
            ),
            Row::bar(
                SettingKey::RawToneMidtoneAnchor,
                "Where the filmic S-curve's midtone line passes through (x, x).",
                MIDTONE_ANCHOR_RANGE,
                ValueFormat::TwoDecimal,
            ),
            Row::check(
                SettingKey::RawDcpToneCurve,
                "Per-camera curve from a matched DCP profile. Skipped by itself for \
                 fuzzy-family matches.",
            ),
        ],
    },
    Group {
        title: Some("Detail"),
        rows: &[
            Row::check(
                SettingKey::RawClarity,
                "Larger-radius unsharp mask on luminance. Lifts midtone features, so the image \
                 reads crisper.",
            ),
            Row::bar(
                SettingKey::RawClarityRadius,
                "Gaussian sigma in pixels for the local-contrast pass. Larger means bigger \
                 features.",
                CLARITY_RADIUS_RANGE,
                ValueFormat::IntegerPx,
            ),
            Row::bar(
                SettingKey::RawClarityAmount,
                "Strength of the local-contrast unsharp mask (0 is off, 1 is aggressive).",
                CLARITY_AMOUNT_RANGE,
                ValueFormat::TwoDecimal,
            ),
            Row::check(
                SettingKey::RawCaptureSharpening,
                "Mild unsharp mask on luminance in display space.",
            ),
            Row::bar(
                SettingKey::RawSharpenAmount,
                "Unsharp-mask strength on the luminance-only capture sharpen pass.",
                SHARPEN_AMOUNT_RANGE,
                ValueFormat::TwoDecimal,
            ),
        ],
    },
    Group {
        title: Some("Denoise"),
        rows: &[Row::check(
            SettingKey::RawChromaDenoise,
            "Mild Gaussian blur on the color channels, keeping luminance sharp.",
        )],
    },
    Group {
        title: Some("Geometry"),
        rows: &[Row::check(
            SettingKey::RawLensCorrection,
            "Distortion, TCA, and vignetting from the LensFun database.",
        )],
    },
    Group {
        title: Some("Output"),
        rows: &[
            Row::check(
                SettingKey::RawHdrOutput,
                "Keep highlights above display white alive when the monitor can show them.",
            ),
            Row::bar(
                SettingKey::RawHdrGain,
                "Multiplier that pushes scene white into the monitor's HDR headroom. 1.0 is \
                 off, 2.0 doubles the brightness.",
                HDR_GAIN_RANGE,
                ValueFormat::TwoDecimal,
            ),
        ],
    },
    Group {
        title: Some("DCP profile"),
        rows: &[Row {
            key: SettingKey::CustomDcpDir,
            kind: RowKind::Folder,
            description: "A folder of .dcp profiles that wins over the ones Prvw ships with and \
                          over Adobe Camera Raw's. Leave it empty to use those.",
            indented: false,
        }],
    },
];

const SLIDESHOW: &[Group] = &[Group {
    title: None,
    rows: &[
        Row {
            key: SettingKey::SlideshowSeconds,
            kind: RowKind::Trackbar(Scale {
                min: MIN_SECONDS as f32,
                max: MAX_SECONDS as f32,
                steps: (MAX_SECONDS - MIN_SECONDS) as i32,
                format: ValueFormat::Seconds,
            }),
            description: "How long each image stays on screen.",
            indented: false,
        },
        Row::check(
            SettingKey::SlideshowCrossfade,
            "Fade from one image to the next instead of cutting.",
        ),
        Row::check(
            SettingKey::SlideshowLoop,
            "Start over at the first image instead of stopping at the last.",
        ),
    ],
}];

const FILE_ASSOCIATIONS: &[Group] = &[Group {
    title: None,
    rows: &[Row {
        key: SettingKey::FileAssociations,
        kind: RowKind::FileTypes,
        description: "Windows itself decides which app opens a file type, and no app can \
                      change that for you. Prvw can put itself on the list, and you pick it in \
                      Windows Settings or through Open with.",
        indented: false,
    }],
}];

// ── Reading and writing values ───────────────────────────────────────────────

/// What a control holds, in the setting's own terms rather than the widget's.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Number(f32),
    Folder(Option<String>),
}

impl Value {
    /// The bool, for a caller that knows the row is a checkbox.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(on) => Some(*on),
            _ => None,
        }
    }

    /// The number, for a caller that knows the row is a trackbar.
    pub fn as_number(&self) -> Option<f32> {
        match self {
            Value::Number(value) => Some(*value),
            _ => None,
        }
    }
}

/// What `key` reads as right now, for painting the control when the dialog opens.
///
/// [`SettingKey::FileAssociations`] has no single value: its surface is a list and two buttons,
/// so it answers `None`.
pub fn value_of(key: SettingKey, settings: &Settings) -> Option<Value> {
    use SettingKey as K;
    let raw = &settings.raw;
    let value = match key {
        K::AutoUpdate => Value::Bool(settings.auto_update),
        K::ScrollToZoom => Value::Bool(settings.scroll_to_zoom),
        K::PreloadNeighbors => Value::Bool(settings.preload_neighbors),
        K::AutoFitWindow => Value::Bool(settings.auto_fit_window),
        K::EnlargeSmallImages => Value::Bool(settings.enlarge_small_images),
        K::IccColorManagement => Value::Bool(settings.icc_color_management),
        K::ColorMatchDisplay => Value::Bool(settings.color_match_display),
        K::RelativeColorimetric => Value::Bool(settings.use_relative_colorimetric),
        K::RawDngOpcodeList1 => Value::Bool(raw.dng_opcode_list_1),
        K::RawDngOpcodeList2 => Value::Bool(raw.dng_opcode_list_2),
        K::RawDngOpcodeList3 => Value::Bool(raw.dng_opcode_list_3),
        K::RawBaselineExposure => Value::Bool(raw.baseline_exposure),
        K::RawBaselineExposureOffset => Value::Number(raw.baseline_exposure_offset),
        K::RawDcpHueSatMap => Value::Bool(raw.dcp_hue_sat_map),
        K::RawDcpLookTable => Value::Bool(raw.dcp_look_table),
        K::RawSaturationBoost => Value::Bool(raw.saturation_boost),
        K::RawSaturationAmount => Value::Number(raw.saturation_boost_amount),
        K::RawHighlightRecovery => Value::Bool(raw.highlight_recovery),
        K::RawDefaultToneCurve => Value::Bool(raw.default_tone_curve),
        K::RawToneMidtoneAnchor => Value::Number(raw.midtone_anchor),
        K::RawDcpToneCurve => Value::Bool(raw.dcp_tone_curve),
        K::RawClarity => Value::Bool(raw.clarity),
        K::RawClarityRadius => Value::Number(raw.clarity_radius),
        K::RawClarityAmount => Value::Number(raw.clarity_amount),
        K::RawCaptureSharpening => Value::Bool(raw.capture_sharpening),
        K::RawSharpenAmount => Value::Number(raw.sharpen_amount),
        K::RawChromaDenoise => Value::Bool(raw.chroma_denoise),
        K::RawLensCorrection => Value::Bool(raw.lens_correction),
        K::RawHdrOutput => Value::Bool(raw.hdr_output),
        K::RawHdrGain => Value::Number(raw.hdr_gain),
        K::CustomDcpDir => Value::Folder(settings.custom_dcp_dir.clone()),
        K::SlideshowSeconds => Value::Number(clamp_seconds(settings.slideshow_seconds) as f32),
        K::SlideshowCrossfade => Value::Bool(settings.slideshow_crossfade),
        K::SlideshowLoop => Value::Bool(settings.slideshow_loop),
        // No dialog row on Windows: `TitleBar` is `NotApplicable` there, and the other four
        // live on the menu bar on every platform.
        K::TitleBar
        | K::FileAssociations
        | K::HistogramVisible
        | K::ExifVisible
        | K::LoopNavigation
        | K::SortBy => return None,
    };
    Some(value)
}

/// What a changed control does: the settings it produces, and how the change reaches the app.
pub struct Change {
    /// `settings` with this one field replaced. The dialog saves it when there's no command.
    pub settings: Settings,
    /// The command that carries the change through the event loop, where one exists.
    ///
    /// `None` means the setting has no `AppCommand` at all, so nothing in the running app
    /// reads it live and persisting it is the whole job. Auto-update is the only one:
    /// `updater` reads it at the next startup. The macOS window does the same thing.
    pub command: Option<AppCommand>,
}

/// Fold a control's new value into the settings, and say how it reaches the app.
///
/// Returns `None` for a key with no dialog row, so a caller can't quietly write a setting the
/// Windows dialog doesn't own.
pub fn apply(key: SettingKey, value: &Value, settings: &Settings) -> Option<Change> {
    use SettingKey as K;
    let mut next = settings.clone();

    // A bool row's value, or bail out: a caller handing a number to a checkbox is a bug here,
    // not something to guess at.
    macro_rules! on {
        () => {
            value.as_bool()?
        };
    }
    macro_rules! number {
        () => {
            value.as_number()?
        };
    }

    let command = match key {
        K::AutoUpdate => {
            next.auto_update = on!();
            None
        }
        K::ScrollToZoom => {
            next.scroll_to_zoom = on!();
            Some(AppCommand::SetScrollToZoom(next.scroll_to_zoom))
        }
        K::PreloadNeighbors => {
            next.preload_neighbors = on!();
            Some(AppCommand::SetPreloadNeighbors(next.preload_neighbors))
        }
        K::AutoFitWindow => {
            next.auto_fit_window = on!();
            Some(AppCommand::SetAutoFitWindow(next.auto_fit_window))
        }
        K::EnlargeSmallImages => {
            next.enlarge_small_images = on!();
            Some(AppCommand::SetEnlargeSmallImages(next.enlarge_small_images))
        }
        K::IccColorManagement => {
            next.icc_color_management = on!();
            Some(AppCommand::SetIccColorManagement(next.icc_color_management))
        }
        K::ColorMatchDisplay => {
            next.color_match_display = on!();
            Some(AppCommand::SetColorMatchDisplay(next.color_match_display))
        }
        K::RelativeColorimetric => {
            next.use_relative_colorimetric = on!();
            Some(AppCommand::SetRelativeColorimetric(
                next.use_relative_colorimetric,
            ))
        }
        K::SlideshowSeconds => {
            next.slideshow_seconds = clamp_seconds(number!().round() as u32);
            Some(AppCommand::SetSlideshowSeconds(next.slideshow_seconds))
        }
        K::SlideshowCrossfade => {
            next.slideshow_crossfade = on!();
            Some(AppCommand::SetSlideshowCrossfade(next.slideshow_crossfade))
        }
        K::SlideshowLoop => {
            next.slideshow_loop = on!();
            Some(AppCommand::SetSlideshowLoop(next.slideshow_loop))
        }
        K::CustomDcpDir => {
            let Value::Folder(folder) = value else {
                return None;
            };
            next.custom_dcp_dir = folder.clone().filter(|path| !path.is_empty());
            Some(AppCommand::SetCustomDcpDir(next.custom_dcp_dir.clone()))
        }
        // Every RAW row rides one command, because the pipeline takes the whole struct.
        K::RawDngOpcodeList1 => {
            next.raw.dng_opcode_list_1 = on!();
            raw_command(&next.raw)
        }
        K::RawDngOpcodeList2 => {
            next.raw.dng_opcode_list_2 = on!();
            raw_command(&next.raw)
        }
        K::RawDngOpcodeList3 => {
            next.raw.dng_opcode_list_3 = on!();
            raw_command(&next.raw)
        }
        K::RawBaselineExposure => {
            next.raw.baseline_exposure = on!();
            raw_command(&next.raw)
        }
        K::RawBaselineExposureOffset => {
            next.raw.baseline_exposure_offset = number!();
            raw_command(&next.raw)
        }
        K::RawDcpHueSatMap => {
            next.raw.dcp_hue_sat_map = on!();
            raw_command(&next.raw)
        }
        K::RawDcpLookTable => {
            next.raw.dcp_look_table = on!();
            raw_command(&next.raw)
        }
        K::RawSaturationBoost => {
            next.raw.saturation_boost = on!();
            raw_command(&next.raw)
        }
        K::RawSaturationAmount => {
            next.raw.saturation_boost_amount = number!();
            raw_command(&next.raw)
        }
        K::RawHighlightRecovery => {
            next.raw.highlight_recovery = on!();
            raw_command(&next.raw)
        }
        K::RawDefaultToneCurve => {
            next.raw.default_tone_curve = on!();
            raw_command(&next.raw)
        }
        K::RawToneMidtoneAnchor => {
            next.raw.midtone_anchor = number!();
            raw_command(&next.raw)
        }
        K::RawDcpToneCurve => {
            next.raw.dcp_tone_curve = on!();
            raw_command(&next.raw)
        }
        K::RawClarity => {
            next.raw.clarity = on!();
            raw_command(&next.raw)
        }
        K::RawClarityRadius => {
            next.raw.clarity_radius = number!();
            raw_command(&next.raw)
        }
        K::RawClarityAmount => {
            next.raw.clarity_amount = number!();
            raw_command(&next.raw)
        }
        K::RawCaptureSharpening => {
            next.raw.capture_sharpening = on!();
            raw_command(&next.raw)
        }
        K::RawSharpenAmount => {
            next.raw.sharpen_amount = number!();
            raw_command(&next.raw)
        }
        K::RawChromaDenoise => {
            next.raw.chroma_denoise = on!();
            raw_command(&next.raw)
        }
        K::RawLensCorrection => {
            next.raw.lens_correction = on!();
            raw_command(&next.raw)
        }
        K::RawHdrOutput => {
            next.raw.hdr_output = on!();
            raw_command(&next.raw)
        }
        K::RawHdrGain => {
            next.raw.hdr_gain = number!();
            raw_command(&next.raw)
        }
        K::TitleBar
        | K::FileAssociations
        | K::HistogramVisible
        | K::ExifVisible
        | K::LoopNavigation
        | K::SortBy => return None,
    };
    Some(Change {
        settings: next,
        command,
    })
}

fn raw_command(raw: &RawPipelineFlags) -> Option<AppCommand> {
    Some(AppCommand::SetRawPipelineFlags(*raw))
}

/// Put the RAW pipeline back to its defaults, for the page's Reset button.
pub fn reset_raw(settings: &Settings) -> Change {
    let mut next = settings.clone();
    next.raw = RawPipelineFlags::default();
    let command = raw_command(&next.raw);
    Change {
        settings: next,
        command,
    }
}

/// The rows that go grey while `key` is off.
///
/// The one cross-dependency the settings have, and it carries over from macOS unchanged:
/// matching the display and picking a rendering intent are both steps inside the ICC
/// transform, so neither means anything with color management off.
pub const fn dependents(key: SettingKey) -> &'static [SettingKey] {
    match key {
        SettingKey::IccColorManagement => &[
            SettingKey::ColorMatchDisplay,
            SettingKey::RelativeColorimetric,
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parity::{Coverage, Platform};
    use serde_json::Value as Json;

    /// The dialog owes exactly what the registry says Windows owes: a row for every panel key
    /// that means something there, and nothing for one that doesn't.
    ///
    /// This is the compile-time guarantee's twin. `parity::Audit` catches the same drift at
    /// runtime when the real dialog opens, which needs a Windows box; this catches it here, on
    /// any host.
    #[test]
    fn every_windows_setting_has_a_row() {
        for (key, coverage) in SettingKey::panel_coverage(Platform::Windows) {
            let row = row_for(key);
            match coverage {
                Coverage::Present => assert!(
                    row.is_some(),
                    "{} is `Present` on Windows with no row in the dialog",
                    key.name()
                ),
                Coverage::Missing | Coverage::NotApplicable { .. } => assert!(
                    row.is_none(),
                    "the dialog builds a row for {}, which Windows declares {}",
                    key.name(),
                    coverage.status()
                ),
            }
        }
    }

    /// A key belongs to one page, and it's the page the registry's `Panel` names. A row on the
    /// wrong tab would pass the coverage check above and still be wrong.
    #[test]
    fn rows_sit_on_the_page_their_panel_names() {
        let mut seen: Vec<SettingKey> = Vec::new();
        for (tab, row) in all_rows() {
            assert_eq!(
                row.key.panel(),
                tab.panel(),
                "{} is a {} setting, but it's on the {} tab",
                row.key.name(),
                row.key.panel().name(),
                tab.title()
            );
            assert!(
                !seen.contains(&row.key),
                "{} has more than one row",
                row.key.name()
            );
            seen.push(row.key);
        }
    }

    /// The registry names the job (`Toggle`, `Slider`, `Path`, `Custom`) and Windows picks the
    /// widget. Picking the wrong one loses the setting: a slider rendered as a checkbox can
    /// only say on or off.
    #[test]
    fn widgets_match_the_control_the_registry_asks_for() {
        use crate::parity::setting_keys::Control;
        for (_, row) in all_rows() {
            let expected_slider = matches!(row.key.control(), Control::Slider);
            assert_eq!(
                matches!(row.kind, RowKind::Trackbar(_)),
                expected_slider,
                "{} asks for a {}",
                row.key.name(),
                row.key.control().name()
            );
            match row.key.control() {
                Control::Toggle => assert_eq!(row.kind, RowKind::Checkbox, "{}", row.key.name()),
                Control::Path => assert_eq!(row.kind, RowKind::Folder, "{}", row.key.name()),
                Control::Custom => assert_eq!(row.kind, RowKind::FileTypes, "{}", row.key.name()),
                Control::Slider | Control::Choice => {}
            }
        }
    }

    /// Win32 eats an ampersand in a control's text as the mnemonic marker, so one in a label
    /// would silently vanish and underline the next letter. Nothing has one; this is what
    /// keeps that true when someone writes new copy.
    #[test]
    fn no_copy_carries_an_ampersand() {
        for (_, row) in all_rows() {
            assert!(!row.label().contains('&'), "{}", row.key.name());
            assert!(!row.description.contains('&'), "{}", row.key.name());
        }
        for text in [SCROLL_TO_ZOOM_ON, SCROLL_TO_ZOOM_OFF] {
            assert!(!text.contains('&'));
        }
    }

    /// Every row says something, and no description is a restatement of the label.
    #[test]
    fn every_row_explains_itself() {
        for (_, row) in all_rows() {
            assert!(
                row.description.len() > 20,
                "{}'s description is too thin: {:?}",
                row.key.name(),
                row.description
            );
            assert_ne!(row.description, row.label(), "{}", row.key.name());
        }
    }

    /// A trackbar's position round-trips back to the same position. Drifting here would walk
    /// a value away from where the user left it every time the dialog reopened.
    #[test]
    fn trackbar_positions_round_trip() {
        for (_, row) in all_rows() {
            let RowKind::Trackbar(scale) = row.kind else {
                continue;
            };
            for position in 0..=scale.steps {
                let value = scale.value(position);
                assert_eq!(
                    scale.position(value),
                    position,
                    "{} drifts at position {position}",
                    row.key.name()
                );
                assert!(
                    value >= scale.min && value <= scale.max,
                    "{} leaves its range at position {position}",
                    row.key.name()
                );
            }
        }
    }

    /// The ends of the track are the ends of the setting's range, and anything outside sticks
    /// to the nearest end rather than wrapping.
    #[test]
    fn trackbar_ends_are_the_settings_ends() {
        let scale = Scale::new((-2.0, 2.0), 100, ValueFormat::TwoDecimal);
        assert_eq!(scale.value(0), -2.0);
        assert_eq!(scale.value(100), 2.0);
        assert_eq!(scale.position(-99.0), 0);
        assert_eq!(scale.position(99.0), 100);
        assert_eq!(scale.value(-5), scale.min);
        assert_eq!(scale.value(500), scale.max);
    }

    /// The slideshow bar is whole seconds, one step each, so the arrow keys move it a second
    /// at a time the way `[` and `]` do.
    #[test]
    fn the_slideshow_bar_counts_seconds() {
        let row = row_for(SettingKey::SlideshowSeconds).expect("the slideshow row exists");
        let RowKind::Trackbar(scale) = row.kind else {
            panic!("time per image is a trackbar");
        };
        assert_eq!(scale.value(0), MIN_SECONDS as f32);
        assert_eq!(scale.value(scale.steps), MAX_SECONDS as f32);
        assert_eq!(scale.render(4.0), "4s");
        assert_eq!(scale.steps, (MAX_SECONDS - MIN_SECONDS) as i32);
    }

    #[test]
    fn value_labels_read_the_way_the_setting_does() {
        assert_eq!(ValueFormat::TwoDecimal.render(0.4), "0.40");
        assert_eq!(ValueFormat::IntegerPx.render(10.2), "10 px");
        assert_eq!(ValueFormat::Seconds.render(4.0), "4s");
    }

    /// Flatten a serialized `Settings` to `dotted.path -> value`, which is the same spelling
    /// `SettingKey::field` uses. A field a key claims outright is a leaf even when it's an
    /// object, so `previous_handlers` doesn't fan out into one entry per file type.
    fn flatten(prefix: &str, value: &Json, out: &mut Vec<(String, Json)>) {
        let claimed = SettingKey::ALL.iter().any(|key| key.field() == prefix);
        match value.as_object() {
            Some(fields) if !claimed && !prefix.is_empty() => {
                for (name, child) in fields {
                    flatten(&format!("{prefix}.{name}"), child, out);
                }
            }
            Some(fields) if prefix.is_empty() => {
                for (name, child) in fields {
                    flatten(name, child, out);
                }
            }
            _ => out.push((prefix.to_string(), value.clone())),
        }
    }

    fn fields(settings: &Settings) -> Vec<(String, Json)> {
        let json = serde_json::to_value(settings).expect("Settings serializes");
        let mut out = Vec::new();
        flatten("", &json, &mut out);
        out
    }

    /// Changing one control writes one field, and it's the field the registry named.
    ///
    /// This is the whole binding layer proved at once: a copy-paste slip that pointed the
    /// clarity radius at `clarity_amount` would pass every other test here and quietly ruin
    /// the RAW page.
    #[test]
    fn a_row_writes_its_own_field_and_nothing_else() {
        let before = Settings::default();
        for (_, row) in all_rows() {
            let Some(current) = value_of(row.key, &before) else {
                // The file-types row has no single value; its own tests are in `file_types`.
                assert_eq!(row.kind, RowKind::FileTypes, "{}", row.key.name());
                continue;
            };
            let changed = match &current {
                Value::Bool(on) => Value::Bool(!on),
                Value::Number(_) => match row.kind {
                    // A position the default can't already be sitting on.
                    RowKind::Trackbar(scale) => {
                        let low = scale.value(1);
                        let high = scale.value(scale.steps - 1);
                        let current = current.as_number().expect("a number");
                        Value::Number(if (current - low).abs() > 0.001 {
                            low
                        } else {
                            high
                        })
                    }
                    _ => panic!("{} is a number without a trackbar", row.key.name()),
                },
                Value::Folder(_) => Value::Folder(Some("C:\\profiles".to_string())),
            };

            let change = apply(row.key, &changed, &before).expect("the row applies");
            let differing: Vec<String> = fields(&before)
                .into_iter()
                .zip(fields(&change.settings))
                .filter(|((_, was), (_, now))| was != now)
                .map(|((name, _), _)| name)
                .collect();
            assert_eq!(
                differing,
                vec![row.key.field().to_string()],
                "{} wrote the wrong fields",
                row.key.name()
            );
        }
    }

    /// The bool a settings command carries, for the test below. Spelled out rather than read
    /// off a `Debug` string, because `AppCommand` has no `Debug`.
    fn bool_carried(command: &AppCommand) -> Option<bool> {
        match command {
            AppCommand::SetScrollToZoom(on)
            | AppCommand::SetPreloadNeighbors(on)
            | AppCommand::SetAutoFitWindow(on)
            | AppCommand::SetEnlargeSmallImages(on)
            | AppCommand::SetIccColorManagement(on)
            | AppCommand::SetColorMatchDisplay(on)
            | AppCommand::SetRelativeColorimetric(on)
            | AppCommand::SetSlideshowCrossfade(on)
            | AppCommand::SetSlideshowLoop(on) => Some(*on),
            _ => None,
        }
    }

    /// Every row that the app reacts to live sends a command, and the command carries the
    /// value that was just set rather than the one before it.
    #[test]
    fn changes_reach_the_app_as_commands() {
        let before = Settings::default();
        for (_, row) in all_rows() {
            let Some(Value::Bool(on)) = value_of(row.key, &before) else {
                continue;
            };
            let change = apply(row.key, &Value::Bool(!on), &before).expect("the row applies");
            if row.key == SettingKey::AutoUpdate {
                // Nothing in the running app reads it, so persisting is the whole job.
                assert!(change.command.is_none());
                continue;
            }
            let command = change.command.expect("a live setting sends a command");
            match command {
                AppCommand::SetRawPipelineFlags(flags) => assert_eq!(flags, change.settings.raw),
                other => assert_eq!(
                    bool_carried(&other),
                    Some(!on),
                    "{} sent the wrong value",
                    row.key.name()
                ),
            }
        }
    }

    /// A checkbox's value handed to a trackbar (or the other way round) is a wiring bug, and
    /// it stops here rather than writing a nonsense number.
    #[test]
    fn a_mismatched_value_changes_nothing() {
        let settings = Settings::default();
        assert!(apply(SettingKey::RawHdrGain, &Value::Bool(true), &settings).is_none());
        assert!(apply(SettingKey::AutoUpdate, &Value::Number(1.0), &settings).is_none());
        // And a key with no row on this platform can't be written through here at all.
        assert!(apply(SettingKey::TitleBar, &Value::Bool(true), &settings).is_none());
        assert!(value_of(SettingKey::SortBy, &settings).is_none());
    }

    #[test]
    fn reset_puts_the_raw_pipeline_back() {
        let mut tuned = Settings::default();
        tuned.raw.clarity = false;
        tuned.raw.hdr_gain = 3.5;
        let change = reset_raw(&tuned);
        assert_eq!(change.settings.raw, RawPipelineFlags::default());
        assert!(matches!(
            change.command,
            Some(AppCommand::SetRawPipelineFlags(_))
        ));
    }

    #[test]
    fn color_match_and_intent_hang_off_icc() {
        assert_eq!(
            dependents(SettingKey::IccColorManagement),
            &[
                SettingKey::ColorMatchDisplay,
                SettingKey::RelativeColorimetric
            ]
        );
        // Enlarge is deliberately not a dependent of auto-fit: auto-fit is inert in
        // fullscreen, where enlarge still governs.
        assert!(dependents(SettingKey::AutoFitWindow).is_empty());
    }

    /// `docs/style-guide.md`, over every string the dialog shows. An em dash is the house's
    /// clearest tell that a machine wrote the copy, and the trivializing words are the ones
    /// that creep back in. `about::content` runs the same sweep over the About box.
    #[test]
    fn the_copy_follows_the_style_guide() {
        for line in user_visible_strings() {
            assert!(!line.contains('\u{2014}'), "em dash in {line:?}");
            let lowercase = line.to_lowercase();
            for banned in ["just ", "simply ", "simple ", "easy "] {
                assert!(!lowercase.contains(banned), "{banned:?} in {line:?}");
            }
        }
    }

    /// Sentence case, which `docs/style-guide.md` asks for in every title and label.
    ///
    /// Titles only: a description is prose, and prose capitalises names and sentence starts
    /// wherever they fall. The first word of a title is skipped for the same reason, so what's
    /// left is a mid-title capital, which has to be a real spelling. The allowlist is the
    /// interesting half of this test: it's short, and everything in it is there on purpose.
    #[test]
    fn titles_are_sentence_case() {
        /// Words that keep a capital mid-title because that's how they're spelled. An
        /// all-uppercase word (DNG, RAW, HDR, ICC) is allowed without being listed.
        const NAMES: &[&str] = &["OpcodeList", "HueSatMap", "LookTable", "Prvw", "Windows"];

        for title in user_visible_titles() {
            for word in title.split_whitespace().skip(1) {
                let word = word.trim_matches(|c: char| !c.is_alphanumeric());
                let root = word.strip_suffix("'s").unwrap_or(word);
                if !root.chars().next().is_some_and(char::is_uppercase) {
                    continue;
                }
                assert!(
                    NAMES.contains(&root) || root.chars().all(|c| !c.is_lowercase()),
                    "{root:?} is capitalised mid-title in {title:?}; add it to NAMES if that's \
                     its real spelling, or lower-case it"
                );
            }
        }
    }

    #[test]
    fn tabs_answer_to_the_names_qa_uses() {
        assert_eq!(Tab::from_section_name("General"), Some(Tab::General));
        assert_eq!(Tab::from_section_name("raw"), Some(Tab::Raw));
        assert_eq!(
            Tab::from_section_name("file_associations"),
            Some(Tab::FileAssociations)
        );
        assert_eq!(Tab::from_section_name("nope"), None);
        for tab in Tab::ALL {
            assert_eq!(Tab::from_section_name(tab.title()), Some(*tab));
        }
    }

    /// The RAW page is the only one tall enough to need scrolling, and the only one with a
    /// Reset button. Both are real costs, so neither should spread by accident.
    #[test]
    fn only_the_raw_page_scrolls() {
        for tab in Tab::ALL {
            let page = page(*tab);
            assert_eq!(page.scrolls, *tab == Tab::Raw);
            assert_eq!(page.reset_button, *tab == Tab::Raw);
            assert_eq!(page.file_types, *tab == Tab::FileAssociations);
        }
    }

    /// The RAW page's group boxes are the macOS panel's section headers, in the same order.
    #[test]
    fn the_raw_page_keeps_the_macos_sections() {
        let titles: Vec<&str> = page(Tab::Raw)
            .groups
            .iter()
            .filter_map(|group| group.title)
            .collect();
        assert_eq!(
            titles,
            vec![
                "Sensor corrections (DNG only)",
                "Color",
                "Tone",
                "Detail",
                "Denoise",
                "Geometry",
                "Output",
                "DCP profile",
            ]
        );
    }

    /// A knob sits under the toggle it belongs to, which is what the indent says. A trackbar
    /// that lost its indent would read as an unrelated setting.
    #[test]
    fn knobs_are_indented_under_their_toggle() {
        for (_, row) in all_rows() {
            if row.key == SettingKey::SlideshowSeconds {
                // The slideshow's own bar is the top-level setting, not a knob under one.
                assert!(!row.indented);
                continue;
            }
            assert_eq!(
                row.indented,
                matches!(row.kind, RowKind::Trackbar(_)),
                "{}",
                row.key.name()
            );
        }
    }
}
