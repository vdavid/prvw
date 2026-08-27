# Platform parity

What every platform's UI owes the app, and what it has. Generated from the registries in `apps/desktop/src/parity/`.

Don't edit this file by hand. Edit the registries, then run `./scripts/check.sh --check parity`, which rewrites it. The
check fails on a stale file, so a parity change shows up as a diff here rather than passing unnoticed.

Statuses: `done` is built and reachable on that platform, `not applicable` means the entry is meaningless there (with
the reason, below), and `missing` means it applies but isn't built. Linux is a long column of `missing` on purpose: it
ships without chrome and gets its own spec later (decision 4 in `docs/specs/cross-platform-plan.md`).

## Summary

- macOS: 117 of 117 done, 0 not applicable, 0 missing
- Windows: 65 of 117 done, 6 not applicable, 46 missing
- Linux: 32 of 117 done, 5 not applicable, 80 missing

Per registry, as `done / not applicable / missing`:

- Settings (40 entries): macOS 40 / 0 / 0, Windows 4 / 1 / 35, Linux 0 / 1 / 39
- Menu items (37 entries): macOS 37 / 0 / 0, Windows 28 / 4 / 5, Linux 0 / 3 / 34
- Commands (40 entries): macOS 40 / 0 / 0, Windows 33 / 1 / 6, Linux 32 / 1 / 7

## What each platform still owes

### macOS

Nothing missing.

### Windows

46 missing:

- Setting, General: `AutoUpdate`, `ScrollToZoom`, `PreloadNeighbors`
- Setting, Zoom: `AutoFitWindow`, `EnlargeSmallImages`
- Setting, Color: `IccColorManagement`, `ColorMatchDisplay`, `RelativeColorimetric`
- Setting, RAW: `RawDngOpcodeList1`, `RawDngOpcodeList2`, `RawDngOpcodeList3`, `RawBaselineExposure`, `RawBaselineExposureOffset`, `RawDcpHueSatMap`, `RawDcpLookTable`, `RawSaturationBoost`, `RawSaturationAmount`, `RawHighlightRecovery`, `RawDefaultToneCurve`, `RawToneMidtoneAnchor`, `RawDcpToneCurve`, `RawClarity`, `RawClarityRadius`, `RawClarityAmount`, `RawCaptureSharpening`, `RawSharpenAmount`, `RawChromaDenoise`, `RawLensCorrection`, `RawHdrOutput`, `RawHdrGain`, `CustomDcpDir`
- Setting, Slideshow: `SlideshowSeconds`, `SlideshowCrossfade`, `SlideshowLoop`
- Setting, File associations: `FileAssociations`
- Menu item, Prvw: `About`, `Settings`
- Menu item, File: `Print`
- Menu item, Navigate: `BrowseToggle`
- Menu item, Context menu: `ContextPrint`
- Command, Browse mode: `BrowseMode`, `BrowseFocus`, `BrowseOpenSelected`
- Command, App: `Print`, `About`, `Settings`

### Linux

80 missing:

- Setting, General: `AutoUpdate`, `ScrollToZoom`, `PreloadNeighbors`
- Setting, Zoom: `AutoFitWindow`, `EnlargeSmallImages`
- Setting, Color: `IccColorManagement`, `ColorMatchDisplay`, `RelativeColorimetric`
- Setting, RAW: `RawDngOpcodeList1`, `RawDngOpcodeList2`, `RawDngOpcodeList3`, `RawBaselineExposure`, `RawBaselineExposureOffset`, `RawDcpHueSatMap`, `RawDcpLookTable`, `RawSaturationBoost`, `RawSaturationAmount`, `RawHighlightRecovery`, `RawDefaultToneCurve`, `RawToneMidtoneAnchor`, `RawDcpToneCurve`, `RawClarity`, `RawClarityRadius`, `RawClarityAmount`, `RawCaptureSharpening`, `RawSharpenAmount`, `RawChromaDenoise`, `RawLensCorrection`, `RawHdrOutput`, `RawHdrGain`, `CustomDcpDir`
- Setting, Slideshow: `SlideshowSeconds`, `SlideshowCrossfade`, `SlideshowLoop`
- Setting, File associations: `FileAssociations`
- Setting, Menu only: `HistogramVisible`, `ExifVisible`, `LoopNavigation`, `SortBy`
- Menu item, Prvw: `About`, `Settings`, `Quit`
- Menu item, File: `Open`, `Print`, `CloseWindow`
- Menu item, Edit: `Copy`
- Menu item, View: `ZoomIn`, `ZoomOut`, `ActualSize`, `FitToWindow`, `AutoFitWindow`, `EnlargeSmallImages`, `IccColorManagement`, `ColorMatchDisplay`, `RelativeColorimetric`, `Histogram`, `ExifInfo`, `SortByName`, `SortByDate`, `SortByFileType`, `Fullscreen`, `Refresh`
- Menu item, Navigate: `BrowseToggle`, `Previous`, `Next`, `GoToFirst`, `GoToLast`, `LoopNavigation`
- Menu item, Slideshow: `SlideshowToggle`, `SlideshowIncreaseSpeed`, `SlideshowDecreaseSpeed`
- Menu item, Context menu: `ContextCopy`, `ContextPrint`
- Command, Browse mode: `BrowseMode`, `BrowseFocus`, `BrowseOpenSelected`
- Command, App: `CopyImage`, `Print`, `About`, `Settings`

## Deliberately not applicable

### Windows

- `TitleBar` (setting): A Win32 client area starts below the caption, so there's no title bar overlapping the image and nothing to reserve space for. macOS needs it because the window draws content behind a transparent title bar.
- `Hide` (menu item): Hiding an app while leaving it running is a macOS app-menu convention. Windows minimizes windows instead, from the window itself rather than a menu.
- `HideOthers` (menu item): Hiding an app while leaving it running is a macOS app-menu convention. Windows minimizes windows instead, from the window itself rather than a menu.
- `ShowAll` (menu item): Hiding an app while leaving it running is a macOS app-menu convention. Windows minimizes windows instead, from the window itself rather than a menu.
- `CloseWindow` (menu item): Prvw has one window on Windows, and a Windows app with no windows is an invisible process rather than a running app. Closing that window is exiting, which File → Exit already does.
- `TitleBar` (command): The title bar never covers the image on Windows, so there's no strip to reserve and nothing for the command to switch.

### Linux

- `TitleBar` (setting): Linux windows carry their decorations outside the surface Prvw draws into, so nothing covers the image and there's no strip to reserve.
- `Hide` (menu item): Hiding an app while leaving it running is a macOS app-menu convention, and no Linux desktop offers the equivalent from an app's own menu.
- `HideOthers` (menu item): Hiding an app while leaving it running is a macOS app-menu convention, and no Linux desktop offers the equivalent from an app's own menu.
- `ShowAll` (menu item): Hiding an app while leaving it running is a macOS app-menu convention, and no Linux desktop offers the equivalent from an app's own menu.
- `TitleBar` (command): Linux decorations sit outside the surface Prvw draws into, so the command has no strip to reserve or release.

## Every entry

### Settings

- `AutoUpdate` "Auto-update" (setting, General, toggle, field `auto_update`): macOS done, Windows missing, Linux missing
- `ScrollToZoom` "Scroll to zoom" (setting, General, toggle, field `scroll_to_zoom`): macOS done, Windows missing, Linux missing
- `PreloadNeighbors` "Preload next/prev images" (setting, General, toggle, field `preload_neighbors`): macOS done, Windows missing, Linux missing
- `TitleBar` "Title bar" (setting, General, toggle, field `title_bar`): macOS done, Windows not applicable, Linux not applicable
- `AutoFitWindow` "Auto-fit window" (setting, Zoom, toggle, field `auto_fit_window`): macOS done, Windows missing, Linux missing
- `EnlargeSmallImages` "Enlarge small images" (setting, Zoom, toggle, field `enlarge_small_images`): macOS done, Windows missing, Linux missing
- `IccColorManagement` "ICC color management" (setting, Color, toggle, field `icc_color_management`): macOS done, Windows missing, Linux missing
- `ColorMatchDisplay` "Color match display" (setting, Color, toggle, field `color_match_display`): macOS done, Windows missing, Linux missing
- `RelativeColorimetric` "Relative colorimetric" (setting, Color, toggle, field `use_relative_colorimetric`): macOS done, Windows missing, Linux missing
- `RawDngOpcodeList1` "DNG OpcodeList 1" (setting, RAW, toggle, field `raw.dng_opcode_list_1`): macOS done, Windows missing, Linux missing
- `RawDngOpcodeList2` "DNG OpcodeList 2" (setting, RAW, toggle, field `raw.dng_opcode_list_2`): macOS done, Windows missing, Linux missing
- `RawDngOpcodeList3` "DNG OpcodeList 3" (setting, RAW, toggle, field `raw.dng_opcode_list_3`): macOS done, Windows missing, Linux missing
- `RawBaselineExposure` "Baseline exposure" (setting, RAW, toggle, field `raw.baseline_exposure`): macOS done, Windows missing, Linux missing
- `RawBaselineExposureOffset` "Baseline exposure offset" (setting, RAW, slider, field `raw.baseline_exposure_offset`): macOS done, Windows missing, Linux missing
- `RawDcpHueSatMap` "DCP HueSatMap" (setting, RAW, toggle, field `raw.dcp_hue_sat_map`): macOS done, Windows missing, Linux missing
- `RawDcpLookTable` "DCP LookTable" (setting, RAW, toggle, field `raw.dcp_look_table`): macOS done, Windows missing, Linux missing
- `RawSaturationBoost` "Saturation boost" (setting, RAW, toggle, field `raw.saturation_boost`): macOS done, Windows missing, Linux missing
- `RawSaturationAmount` "Saturation amount" (setting, RAW, slider, field `raw.saturation_boost_amount`): macOS done, Windows missing, Linux missing
- `RawHighlightRecovery` "Highlight recovery" (setting, RAW, toggle, field `raw.highlight_recovery`): macOS done, Windows missing, Linux missing
- `RawDefaultToneCurve` "Default tone curve" (setting, RAW, toggle, field `raw.default_tone_curve`): macOS done, Windows missing, Linux missing
- `RawToneMidtoneAnchor` "Tone midtone anchor" (setting, RAW, slider, field `raw.midtone_anchor`): macOS done, Windows missing, Linux missing
- `RawDcpToneCurve` "DCP tone curve" (setting, RAW, toggle, field `raw.dcp_tone_curve`): macOS done, Windows missing, Linux missing
- `RawClarity` "Clarity (local contrast)" (setting, RAW, toggle, field `raw.clarity`): macOS done, Windows missing, Linux missing
- `RawClarityRadius` "Clarity radius" (setting, RAW, slider, field `raw.clarity_radius`): macOS done, Windows missing, Linux missing
- `RawClarityAmount` "Clarity amount" (setting, RAW, slider, field `raw.clarity_amount`): macOS done, Windows missing, Linux missing
- `RawCaptureSharpening` "Capture sharpening" (setting, RAW, toggle, field `raw.capture_sharpening`): macOS done, Windows missing, Linux missing
- `RawSharpenAmount` "Sharpening amount" (setting, RAW, slider, field `raw.sharpen_amount`): macOS done, Windows missing, Linux missing
- `RawChromaDenoise` "Chroma noise reduction" (setting, RAW, toggle, field `raw.chroma_denoise`): macOS done, Windows missing, Linux missing
- `RawLensCorrection` "Lens correction" (setting, RAW, toggle, field `raw.lens_correction`): macOS done, Windows missing, Linux missing
- `RawHdrOutput` "HDR / EDR output" (setting, RAW, toggle, field `raw.hdr_output`): macOS done, Windows missing, Linux missing
- `RawHdrGain` "HDR brightness gain" (setting, RAW, slider, field `raw.hdr_gain`): macOS done, Windows missing, Linux missing
- `CustomDcpDir` "Custom DCP directory" (setting, RAW, path, field `custom_dcp_dir`): macOS done, Windows missing, Linux missing
- `SlideshowSeconds` "Time per image" (setting, Slideshow, slider, field `slideshow_seconds`): macOS done, Windows missing, Linux missing
- `SlideshowCrossfade` "Crossfade" (setting, Slideshow, toggle, field `slideshow_crossfade`): macOS done, Windows missing, Linux missing
- `SlideshowLoop` "Loop" (setting, Slideshow, toggle, field `slideshow_loop`): macOS done, Windows missing, Linux missing
- `FileAssociations` "File associations" (setting, File associations, custom, field `previous_handlers`): macOS done, Windows missing, Linux missing
- `HistogramVisible` "Histogram" (setting, Menu only, toggle, field `histogram_visible`): macOS done, Windows done, Linux missing
- `ExifVisible` "Exif info" (setting, Menu only, toggle, field `exif_visible`): macOS done, Windows done, Linux missing
- `LoopNavigation` "Loop navigation" (setting, Menu only, toggle, field `loop_navigation`): macOS done, Windows done, Linux missing
- `SortBy` "Sort by" (setting, Menu only, choice, field `sort_by`): macOS done, Windows done, Linux missing

### Menu items

- `About` "About Prvw" (menu item, Prvw menu): macOS done, Windows missing, Linux missing
- `Settings` "Settings…" (menu item, Prvw menu): macOS done, Windows missing, Linux missing
- `Hide` "Hide Prvw" (menu item, Prvw menu): macOS done, Windows not applicable, Linux not applicable
- `HideOthers` "Hide others" (menu item, Prvw menu): macOS done, Windows not applicable, Linux not applicable
- `ShowAll` "Show all" (menu item, Prvw menu): macOS done, Windows not applicable, Linux not applicable
- `Quit` "Quit Prvw" (menu item, Prvw menu): macOS done, Windows done, Linux missing
- `Open` "Open…" (menu item, File menu): macOS done, Windows done, Linux missing
- `Print` "Print…" (menu item, File menu): macOS done, Windows missing, Linux missing
- `CloseWindow` "Close window" (menu item, File menu): macOS done, Windows not applicable, Linux missing
- `Copy` "Copy image" (menu item, Edit menu): macOS done, Windows done, Linux missing
- `ZoomIn` "Zoom in" (menu item, View menu): macOS done, Windows done, Linux missing
- `ZoomOut` "Zoom out" (menu item, View menu): macOS done, Windows done, Linux missing
- `ActualSize` "Actual size" (menu item, View menu): macOS done, Windows done, Linux missing
- `FitToWindow` "Fit to window" (menu item, View menu): macOS done, Windows done, Linux missing
- `AutoFitWindow` "Auto-fit window" (menu item, View menu): macOS done, Windows done, Linux missing
- `EnlargeSmallImages` "Enlarge small images" (menu item, View menu): macOS done, Windows done, Linux missing
- `IccColorManagement` "ICC color management" (menu item, View menu): macOS done, Windows done, Linux missing
- `ColorMatchDisplay` "Color match display" (menu item, View menu): macOS done, Windows done, Linux missing
- `RelativeColorimetric` "Relative colorimetric" (menu item, View menu): macOS done, Windows done, Linux missing
- `Histogram` "Histogram" (menu item, View menu): macOS done, Windows done, Linux missing
- `ExifInfo` "Exif info" (menu item, View menu): macOS done, Windows done, Linux missing
- `SortByName` "Name" (menu item, View menu): macOS done, Windows done, Linux missing
- `SortByDate` "Date" (menu item, View menu): macOS done, Windows done, Linux missing
- `SortByFileType` "File type" (menu item, View menu): macOS done, Windows done, Linux missing
- `Fullscreen` "Fullscreen" (menu item, View menu): macOS done, Windows done, Linux missing
- `Refresh` "Refresh" (menu item, View menu): macOS done, Windows done, Linux missing
- `BrowseToggle` "Image browser" (menu item, Navigate menu): macOS done, Windows missing, Linux missing
- `Previous` "Previous" (menu item, Navigate menu): macOS done, Windows done, Linux missing
- `Next` "Next" (menu item, Navigate menu): macOS done, Windows done, Linux missing
- `GoToFirst` "Go to first" (menu item, Navigate menu): macOS done, Windows done, Linux missing
- `GoToLast` "Go to last" (menu item, Navigate menu): macOS done, Windows done, Linux missing
- `LoopNavigation` "Loop navigation" (menu item, Navigate menu): macOS done, Windows done, Linux missing
- `SlideshowToggle` "Start slideshow" (menu item, Slideshow menu): macOS done, Windows done, Linux missing
- `SlideshowIncreaseSpeed` "Increase speed" (menu item, Slideshow menu): macOS done, Windows done, Linux missing
- `SlideshowDecreaseSpeed` "Decrease speed" (menu item, Slideshow menu): macOS done, Windows done, Linux missing
- `ContextCopy` "Copy image" (menu item, Context menu): macOS done, Windows done, Linux missing
- `ContextPrint` "Print…" (menu item, Context menu): macOS done, Windows missing, Linux missing

### Commands

- `NextPreviousImage` "Next / previous image" (command, Navigation): macOS done, Windows done, Linux done
- `GoToFirst` "Go to first" (command, Navigation): macOS done, Windows done, Linux done
- `GoToLast` "Go to last" (command, Navigation): macOS done, Windows done, Linux done
- `OpenFile` "Open a file" (command, Navigation): macOS done, Windows done, Linux done
- `DropToOpen` "Drop files onto the window" (command, Navigation): macOS done, Windows done, Linux done
- `LoopNavigation` "Loop navigation" (command, Navigation): macOS done, Windows done, Linux done
- `SortBy` "Sort by" (command, Navigation): macOS done, Windows done, Linux done
- `Refresh` "Refresh" (command, Navigation): macOS done, Windows done, Linux done
- `ZoomIn` "Zoom in" (command, View): macOS done, Windows done, Linux done
- `ZoomOut` "Zoom out" (command, View): macOS done, Windows done, Linux done
- `SetZoom` "Set zoom level" (command, View): macOS done, Windows done, Linux done
- `FitToWindow` "Fit to window" (command, View): macOS done, Windows done, Linux done
- `ActualSize` "Actual size" (command, View): macOS done, Windows done, Linux done
- `ToggleFit` "Toggle fit and actual size" (command, View): macOS done, Windows done, Linux done
- `Fullscreen` "Fullscreen" (command, View): macOS done, Windows done, Linux done
- `AutoFitWindow` "Auto-fit window" (command, View): macOS done, Windows done, Linux done
- `EnlargeSmallImages` "Enlarge small images" (command, View): macOS done, Windows done, Linux done
- `IccColorManagement` "ICC color management" (command, View): macOS done, Windows done, Linux done
- `ColorMatchDisplay` "Color match display" (command, View): macOS done, Windows done, Linux done
- `RelativeColorimetric` "Relative colorimetric" (command, View): macOS done, Windows done, Linux done
- `ScrollToZoom` "Scroll to zoom" (command, View): macOS done, Windows done, Linux done
- `PreloadNeighbors` "Preload next/prev images" (command, View): macOS done, Windows done, Linux done
- `TitleBar` "Title bar" (command, View): macOS done, Windows not applicable, Linux not applicable
- `Histogram` "Histogram" (command, View): macOS done, Windows done, Linux done
- `ExifInfo` "Exif info" (command, View): macOS done, Windows done, Linux done
- `BrowseMode` "Image browser and image view" (command, Browse mode): macOS done, Windows missing, Linux missing
- `BrowseFocus` "Move focus between tree and grid" (command, Browse mode): macOS done, Windows missing, Linux missing
- `BrowseOpenSelected` "Open the selected image" (command, Browse mode): macOS done, Windows missing, Linux missing
- `Slideshow` "Start / stop slideshow" (command, Slideshow): macOS done, Windows done, Linux done
- `SlideshowSeconds` "Time per image" (command, Slideshow): macOS done, Windows done, Linux done
- `SlideshowCrossfade` "Crossfade" (command, Slideshow): macOS done, Windows done, Linux done
- `SlideshowLoop` "Loop the slideshow" (command, Slideshow): macOS done, Windows done, Linux done
- `SlideshowSpeed` "Increase / decrease speed" (command, Slideshow): macOS done, Windows done, Linux done
- `RawPipelineFlags` "RAW pipeline stages" (command, RAW): macOS done, Windows done, Linux done
- `CustomDcpDir` "Custom DCP directory" (command, RAW): macOS done, Windows done, Linux done
- `CopyImage` "Copy image" (command, App): macOS done, Windows done, Linux missing
- `Print` "Print" (command, App): macOS done, Windows missing, Linux missing
- `About` "About Prvw" (command, App): macOS done, Windows missing, Linux missing
- `Settings` "Settings window" (command, App): macOS done, Windows missing, Linux missing
- `Exit` "Exit" (command, App): macOS done, Windows done, Linux done
