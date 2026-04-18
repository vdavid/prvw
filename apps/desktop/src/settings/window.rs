//! Settings window — entry point and `SettingsDelegate`.
//!
//! Sidebar + four panels (General, Zoom, Color, File associations). Each panel is
//! built by its own submodule under `panels/`; this file stitches the pieces together
//! and owns the delegate that handles section switching plus cross-panel dependencies
//! (ICC → Color match / Relative colorimetric; Auto-fit → Enlarge).

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezelStyle, NSButton, NSColor, NSControlStateValueOff,
    NSControlStateValueOn, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSStackView,
    NSSwitch, NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::platform::macos::ui_common::{
    FlippedView, add_vibrancy_background, as_view, center_window, is_window_already_open,
    make_close_button, make_escape_button,
};

// ─── Delegate ─────────────────────────────────────────────────────────────

struct SettingsDelegateIvars {
    enlarge_toggle: *const NSSwitch,
    color_match_toggle: *const NSSwitch,
    relative_col_toggle: *const NSSwitch,
    scroll_to_zoom_desc: *const NSTextField,
    general_panel: *const NSStackView,
    zoom_panel: *const NSStackView,
    color_panel: *const NSStackView,
    raw_panel: *const NSStackView,
    file_assoc_panel: *const NSStackView,
    sidebar_general_btn: *const NSButton,
    sidebar_zoom_btn: *const NSButton,
    sidebar_color_btn: *const NSButton,
    sidebar_raw_btn: *const NSButton,
    sidebar_file_assoc_btn: *const NSButton,
}

// SAFETY: Raw pointers are only used on the main thread within the window's lifetime,
// and the pointed-to objects are kept alive by retained_views.
unsafe impl Send for SettingsDelegateIvars {}
unsafe impl Sync for SettingsDelegateIvars {}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This type doesn't impl Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrvwSettingsDelegate"]
    #[ivars = SettingsDelegateIvars]
    struct SettingsDelegate;

    unsafe impl NSObjectProtocol for SettingsDelegate {}

    impl SettingsDelegate {
        #[unsafe(method(toggleAutoUpdate:))]
        fn toggle_auto_update(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Auto-update toggled: {on}");
            let mut settings = crate::settings::Settings::load();
            settings.auto_update = on;
            settings.save();
        }

        #[unsafe(method(toggleAutoFitWindow:))]
        fn toggle_auto_fit_window(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Auto-fit window toggled via settings: {on}");
            crate::commands::send_command(crate::commands::AppCommand::SetAutoFitWindow(on));
            unsafe {
                let enlarge = self.ivars().enlarge_toggle;
                if !enlarge.is_null() {
                    let _: () = msg_send![enlarge, setEnabled: !on];
                }
            }
        }

        #[unsafe(method(toggleEnlargeSmallImages:))]
        fn toggle_enlarge_small_images(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Enlarge small images toggled via settings: {on}");
            crate::commands::send_command(
                crate::commands::AppCommand::SetEnlargeSmallImages(on),
            );
        }

        #[unsafe(method(toggleIccColorManagement:))]
        fn toggle_icc_color_management(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("ICC color management toggled via settings: {on}");
            crate::commands::send_command(
                crate::commands::AppCommand::SetIccColorManagement(on),
            );
            unsafe {
                let cm = self.ivars().color_match_toggle;
                if !cm.is_null() {
                    let _: () = msg_send![cm, setEnabled: on];
                }
                let rc = self.ivars().relative_col_toggle;
                if !rc.is_null() {
                    let _: () = msg_send![rc, setEnabled: on];
                }
            }
        }

        #[unsafe(method(toggleColorMatchDisplay:))]
        fn toggle_color_match_display(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Color match display toggled via settings: {on}");
            crate::commands::send_command(
                crate::commands::AppCommand::SetColorMatchDisplay(on),
            );
        }

        #[unsafe(method(toggleRelativeColorimetric:))]
        fn toggle_relative_colorimetric(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Relative colorimetric toggled via settings: {on}");
            crate::commands::send_command(
                crate::commands::AppCommand::SetRelativeColorimetric(on),
            );
        }

        #[unsafe(method(toggleScrollToZoom:))]
        fn toggle_scroll_to_zoom(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Scroll to zoom toggled via settings: {on}");
            crate::commands::send_command(crate::commands::AppCommand::SetScrollToZoom(on));
            unsafe {
                let desc = self.ivars().scroll_to_zoom_desc;
                if !desc.is_null() {
                    let text = if on {
                        "Use scroll to zoom instead of switching images."
                    } else {
                        "You can still zoom with trackpad pinch and \u{2318}+/\u{2318}\u{2212}."
                    };
                    let _: () = msg_send![desc, setStringValue: &*NSString::from_str(text)];
                }
            }
        }

        #[unsafe(method(toggleTitleBar:))]
        fn toggle_title_bar(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Title bar toggled via settings: {on}");
            crate::commands::send_command(crate::commands::AppCommand::SetTitleBar(on));
        }

        #[unsafe(method(togglePreloadNeighbors:))]
        fn toggle_preload_neighbors(&self, sender: &NSSwitch) {
            let on = sender.state() == NSControlStateValueOn;
            log::debug!("Preload neighbors toggled via settings: {on}");
            crate::commands::send_command(
                crate::commands::AppCommand::SetPreloadNeighbors(on),
            );
        }

        #[unsafe(method(selectGeneral:))]
        fn select_general(&self, _sender: &AnyObject) {
            self.select_panel(0);
        }

        #[unsafe(method(selectZoom:))]
        fn select_zoom(&self, _sender: &AnyObject) {
            self.select_panel(1);
        }

        #[unsafe(method(selectColor:))]
        fn select_color(&self, _sender: &AnyObject) {
            self.select_panel(2);
        }

        #[unsafe(method(selectRaw:))]
        fn select_raw(&self, _sender: &AnyObject) {
            self.select_panel(3);
        }

        #[unsafe(method(selectFileAssoc:))]
        fn select_file_assoc(&self, _sender: &AnyObject) {
            self.select_panel(4);
        }
    }
);

impl SettingsDelegate {
    fn new(mtm: MainThreadMarker, ivars: SettingsDelegateIvars) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn select_panel(&self, index: usize) {
        let ivars = self.ivars();
        let panels = [
            ivars.general_panel,
            ivars.zoom_panel,
            ivars.color_panel,
            ivars.raw_panel,
            ivars.file_assoc_panel,
        ];
        let buttons = [
            ivars.sidebar_general_btn,
            ivars.sidebar_zoom_btn,
            ivars.sidebar_color_btn,
            ivars.sidebar_raw_btn,
            ivars.sidebar_file_assoc_btn,
        ];
        for (i, &panel) in panels.iter().enumerate() {
            if !panel.is_null() {
                let hidden = i != index;
                unsafe {
                    let _: () = msg_send![panel, setHidden: hidden];
                }
            }
        }
        for (i, &btn) in buttons.iter().enumerate() {
            if !btn.is_null() {
                let state = if i == index {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                };
                unsafe {
                    let _: () = msg_send![btn, setState: state];
                }
            }
        }
    }
}

// ─── Window construction ──────────────────────────────────────────────────

/// Show the Settings window as a non-modal NSWindow.
///
/// Takes an optional parent NSWindow pointer to center on. The window is shown
/// immediately and returns without blocking.
///
/// # Safety
/// `parent_ns_window` must be a valid NSWindow pointer or null.
pub fn show_settings_window(parent_ns_window: *const NSWindow) {
    if is_window_already_open("Settings") {
        return;
    }

    // SAFETY: we're on the main thread (called from winit event handler)
    let mtm = unsafe { MainThreadMarker::new_unchecked() };

    // ── Create the window ──────────────────────────────────────────────

    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;

    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(620.0, 520.0));

    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            content_rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );

        window.setTitle(&NSString::from_str("Settings"));
        let _: () = msg_send![&*window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![&*window, setMovableByWindowBackground: true];
        let _: () = msg_send![&*window, setReleasedWhenClosed: false];

        window.setMinSize(NSSize::new(500.0, 400.0));
        window.setMaxSize(NSSize::new(900.0, 800.0));

        window
    };

    // ── Build content views ────────────────────────────────────────────

    let mut retained_views: Vec<Retained<AnyObject>> = Vec::new();

    add_vibrancy_background(&window, mtm, &mut retained_views);

    let settings = crate::settings::Settings::load();
    let content_max_width = 400.0;

    // ── Sidebar buttons ───────────────────────────────────────────────

    let mut make_sidebar_button = |title: &str| -> Retained<NSButton> {
        unsafe {
            let btn = NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                None,
                None,
                mtm,
            );
            btn.setBezelStyle(NSBezelStyle::AccessoryBarAction);
            let _: () = msg_send![&*btn, setButtonType: 1i64]; // PushOnPushOff
            let _: () = msg_send![&*btn, setAlignment: 0i64]; // NSTextAlignmentLeft

            // Fixed width
            let w = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &btn, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute,
                1.0, 140.0,
            );
            w.setActive(true);
            retained_views.push(Retained::cast_unchecked(w));

            btn
        }
    };

    let sidebar_general_btn = make_sidebar_button("General");
    let sidebar_zoom_btn = make_sidebar_button("Zoom");
    let sidebar_color_btn = make_sidebar_button("Color");
    let sidebar_raw_btn = make_sidebar_button("RAW");
    let sidebar_file_assoc_btn = make_sidebar_button("File associations");

    // General starts selected
    sidebar_general_btn.setState(NSControlStateValueOn);

    let sidebar_stack = NSStackView::new(mtm);
    sidebar_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    sidebar_stack.setAlignment(NSLayoutAttribute::Leading);
    sidebar_stack.setSpacing(12.0);
    sidebar_stack.addArrangedSubview(unsafe { as_view::<NSButton>(&sidebar_general_btn) });
    sidebar_stack.addArrangedSubview(unsafe { as_view::<NSButton>(&sidebar_zoom_btn) });
    sidebar_stack.addArrangedSubview(unsafe { as_view::<NSButton>(&sidebar_color_btn) });
    sidebar_stack.addArrangedSubview(unsafe { as_view::<NSButton>(&sidebar_raw_btn) });
    sidebar_stack.addArrangedSubview(unsafe { as_view::<NSButton>(&sidebar_file_assoc_btn) });

    // ── Build each panel ──────────────────────────────────────────────

    let general =
        super::panels::general::build(&settings, content_max_width, &mut retained_views, mtm);
    let zoom =
        crate::zoom::settings_panel::build(&settings, content_max_width, &mut retained_views, mtm);
    let color =
        crate::color::settings_panel::build(&settings, content_max_width, &mut retained_views, mtm);
    let raw = super::panels::raw::build(&settings, content_max_width, &mut retained_views, mtm);
    let file_assoc_panel =
        crate::file_associations::settings_panel::build(&mut retained_views, mtm);

    // ── Create the main settings delegate with refs to panel widgets ──

    let ivars = SettingsDelegateIvars {
        enlarge_toggle: &*zoom.enlarge_toggle as *const NSSwitch,
        color_match_toggle: &*color.cm_toggle as *const NSSwitch,
        relative_col_toggle: &*color.rc_toggle as *const NSSwitch,
        scroll_to_zoom_desc: &*general.scroll_to_zoom_desc as *const NSTextField,
        general_panel: &*general.panel as *const NSStackView,
        zoom_panel: &*zoom.panel as *const NSStackView,
        color_panel: &*color.panel as *const NSStackView,
        raw_panel: &*raw.panel as *const NSStackView,
        file_assoc_panel: &*file_assoc_panel as *const NSStackView,
        sidebar_general_btn: &*sidebar_general_btn as *const NSButton,
        sidebar_zoom_btn: &*sidebar_zoom_btn as *const NSButton,
        sidebar_color_btn: &*sidebar_color_btn as *const NSButton,
        sidebar_raw_btn: &*sidebar_raw_btn as *const NSButton,
        sidebar_file_assoc_btn: &*sidebar_file_assoc_btn as *const NSButton,
    };
    let delegate = SettingsDelegate::new(mtm, ivars);

    // ── Wire target/action on all controls ────────────────────────────

    unsafe {
        general
            .auto_update_toggle
            .setTarget(Some(&delegate as &AnyObject));
        general
            .auto_update_toggle
            .setAction(Some(sel!(toggleAutoUpdate:)));

        general
            .scroll_to_zoom_toggle
            .setTarget(Some(&delegate as &AnyObject));
        general
            .scroll_to_zoom_toggle
            .setAction(Some(sel!(toggleScrollToZoom:)));

        general
            .preload_neighbors_toggle
            .setTarget(Some(&delegate as &AnyObject));
        general
            .preload_neighbors_toggle
            .setAction(Some(sel!(togglePreloadNeighbors:)));

        general
            .title_bar_toggle
            .setTarget(Some(&delegate as &AnyObject));
        general
            .title_bar_toggle
            .setAction(Some(sel!(toggleTitleBar:)));

        zoom.auto_fit_toggle
            .setTarget(Some(&delegate as &AnyObject));
        zoom.auto_fit_toggle
            .setAction(Some(sel!(toggleAutoFitWindow:)));

        zoom.enlarge_toggle.setTarget(Some(&delegate as &AnyObject));
        zoom.enlarge_toggle
            .setAction(Some(sel!(toggleEnlargeSmallImages:)));

        color.icc_toggle.setTarget(Some(&delegate as &AnyObject));
        color
            .icc_toggle
            .setAction(Some(sel!(toggleIccColorManagement:)));

        color.cm_toggle.setTarget(Some(&delegate as &AnyObject));
        color
            .cm_toggle
            .setAction(Some(sel!(toggleColorMatchDisplay:)));

        color.rc_toggle.setTarget(Some(&delegate as &AnyObject));
        color
            .rc_toggle
            .setAction(Some(sel!(toggleRelativeColorimetric:)));

        sidebar_general_btn.setTarget(Some(&delegate as &AnyObject));
        sidebar_general_btn.setAction(Some(sel!(selectGeneral:)));

        sidebar_zoom_btn.setTarget(Some(&delegate as &AnyObject));
        sidebar_zoom_btn.setAction(Some(sel!(selectZoom:)));

        sidebar_color_btn.setTarget(Some(&delegate as &AnyObject));
        sidebar_color_btn.setAction(Some(sel!(selectColor:)));

        sidebar_raw_btn.setTarget(Some(&delegate as &AnyObject));
        sidebar_raw_btn.setAction(Some(sel!(selectRaw:)));

        sidebar_file_assoc_btn.setTarget(Some(&delegate as &AnyObject));
        sidebar_file_assoc_btn.setAction(Some(sel!(selectFileAssoc:)));
    }

    // ── Close + ESC buttons ───────────────────────────────────────────

    let close_button = make_close_button("Close", &window, mtm);
    let esc_button = make_escape_button(&window, mtm);

    // ── Layout with Auto Layout ───────────────────────────────────────

    unsafe {
        let content_view: *mut NSView = msg_send![&*window, contentView];
        let content_view_ref = &*content_view;
        let content_view_retained = Retained::retain(content_view).unwrap();

        // Sidebar
        let _: () = msg_send![&*sidebar_stack, setTranslatesAutoresizingMaskIntoConstraints: false];
        content_view_ref.addSubview(as_view::<NSStackView>(&sidebar_stack));

        // Separator line
        let separator = FlippedView::new_as_nsview(mtm);
        let _: () = msg_send![&*separator, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*separator, setWantsLayer: true];
        let sep_layer: *const AnyObject = msg_send![&*separator, layer];
        if !sep_layer.is_null() {
            // Use raw objc_msgSend for CGColor — it returns a CGColorRef (opaque C pointer)
            // which msg_send! mis-encodes as '@' (ObjC object) instead of '^{CGColor=}'.
            let sep_color = NSColor::separatorColor();
            let cg_color_sel = sel!(CGColor);
            let bg_color_sel = sel!(setBackgroundColor:);
            let get_cg: unsafe extern "C" fn(
                *const AnyObject,
                objc2::runtime::Sel,
            ) -> *const std::ffi::c_void =
                std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            let cg_color = get_cg(&*sep_color as *const _ as *const AnyObject, cg_color_sel);
            let set_bg: unsafe extern "C" fn(
                *const AnyObject,
                objc2::runtime::Sel,
                *const std::ffi::c_void,
            ) = std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            set_bg(sep_layer, bg_color_sel, cg_color);
        }
        content_view_ref.addSubview(&separator);

        // Content container (holds both panels, pinned to same edges)
        let content_container = FlippedView::new_as_nsview(mtm);
        let _: () =
            msg_send![&*content_container, setTranslatesAutoresizingMaskIntoConstraints: false];

        // Wrap content in a scroll view so tall sections are scrollable
        let scroll_view: Retained<AnyObject> = {
            let sv: Retained<AnyObject> = msg_send![objc2::class!(NSScrollView), new];
            let _: () = msg_send![&*sv, setTranslatesAutoresizingMaskIntoConstraints: false];
            let _: () = msg_send![&*sv, setDocumentView: &*content_container];
            let _: () = msg_send![&*sv, setHasVerticalScroller: true];
            let _: () = msg_send![&*sv, setHasHorizontalScroller: false];
            let _: () = msg_send![&*sv, setAutohidesScrollers: true];
            let _: () = msg_send![&*sv, setDrawsBackground: false];
            let _: () = msg_send![&*sv, setBorderType: 0i64]; // NSNoBorder
            sv
        };
        content_view_ref.addSubview(&*(&*scroll_view as *const AnyObject as *const NSView));

        let clip_view: *const AnyObject = msg_send![&*scroll_view, contentView];

        // Pin content_container width to the clip view so it doesn't scroll horizontally.
        // Top-anchoring is handled by FlippedView (Y=0 at top).
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &content_container, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
            Some(&*clip_view as &AnyObject), NSLayoutAttribute::Width, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Add panels to content container
        let _: () = msg_send![&*general.panel, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*zoom.panel, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*color.panel, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*raw.panel, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () =
            msg_send![&*file_assoc_panel, setTranslatesAutoresizingMaskIntoConstraints: false];
        content_container.addSubview(as_view::<NSStackView>(&general.panel));
        content_container.addSubview(as_view::<NSStackView>(&zoom.panel));
        content_container.addSubview(as_view::<NSStackView>(&color.panel));
        content_container.addSubview(as_view::<NSStackView>(&raw.panel));
        content_container.addSubview(as_view::<NSStackView>(&file_assoc_panel));

        // Horizontal separator above Close button
        let close_separator = FlippedView::new_as_nsview(mtm);
        let _: () =
            msg_send![&*close_separator, setTranslatesAutoresizingMaskIntoConstraints: false];
        let _: () = msg_send![&*close_separator, setWantsLayer: true];
        let close_sep_layer: *const AnyObject = msg_send![&*close_separator, layer];
        if !close_sep_layer.is_null() {
            let sep_color = NSColor::separatorColor();
            let cg_color_sel = sel!(CGColor);
            let bg_color_sel = sel!(setBackgroundColor:);
            let get_cg: unsafe extern "C" fn(
                *const AnyObject,
                objc2::runtime::Sel,
            ) -> *const std::ffi::c_void =
                std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            let cg_color = get_cg(&*sep_color as *const _ as *const AnyObject, cg_color_sel);
            let set_bg: unsafe extern "C" fn(
                *const AnyObject,
                objc2::runtime::Sel,
                *const std::ffi::c_void,
            ) = std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            set_bg(close_sep_layer, bg_color_sel, cg_color);
        }
        content_view_ref.addSubview(&close_separator);

        // Close button below panels
        let _: () = msg_send![&*close_button, setTranslatesAutoresizingMaskIntoConstraints: false];
        content_view_ref.addSubview(as_view::<NSButton>(&close_button));

        // ESC button (hidden)
        content_view_ref.addSubview(as_view::<NSButton>(&esc_button));

        // ── Constraints ───────────────────────────────────────────────

        // Sidebar: top, leading, bottom, fixed width
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &sidebar_stack, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top, 1.0, 36.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &sidebar_stack, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Leading, 1.0, 8.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &sidebar_stack, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 150.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Separator: 1px wide, after sidebar
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &separator, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
            Some(as_view::<NSStackView>(&sidebar_stack) as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &separator, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top, 1.0, 36.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &separator, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &separator, NSLayoutAttribute::Width, NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 1.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Scroll view: after vertical separator, top, trailing, above horizontal separator
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &scroll_view, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
            Some(&separator as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &scroll_view, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top, 1.0, 36.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &scroll_view, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &scroll_view, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(&close_separator as &AnyObject), NSLayoutAttribute::Top, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Horizontal separator: from vertical separator to right edge, 1px tall, above close button
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_separator, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
            Some(&separator as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_separator, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Trailing, 1.0, 0.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_separator, NSLayoutAttribute::Height, NSLayoutRelation::Equal,
            None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute, 1.0, 1.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_separator, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(as_view::<NSButton>(&close_button) as &AnyObject), NSLayoutAttribute::Top, 1.0, -12.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        // Pin each panel to content container edges
        for panel in [
            &general.panel,
            &zoom.panel,
            &color.panel,
            &raw.panel,
            &file_assoc_panel,
        ] {
            let panel_view = as_view::<NSStackView>(panel);

            let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                panel_view, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
                Some(&content_container as &AnyObject), NSLayoutAttribute::Top, 1.0, 0.0,
            );
            c.setActive(true);
            retained_views.push(Retained::cast_unchecked(c));

            let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                panel_view, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
                Some(&content_container as &AnyObject), NSLayoutAttribute::Leading, 1.0, 20.0,
            );
            c.setActive(true);
            retained_views.push(Retained::cast_unchecked(c));

            let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                panel_view, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
                Some(&content_container as &AnyObject), NSLayoutAttribute::Trailing, 1.0, -20.0,
            );
            c.setActive(true);
            retained_views.push(Retained::cast_unchecked(c));

            // Bottom: content_container must be at least as tall as the panel (for scroll view sizing)
            let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &content_container, NSLayoutAttribute::Bottom, NSLayoutRelation::GreaterThanOrEqual,
                Some(panel_view as &AnyObject), NSLayoutAttribute::Bottom, 1.0, 16.0,
            );
            c.setActive(true);
            retained_views.push(Retained::cast_unchecked(c));
        }

        // Close button: bottom-right
        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_button, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Trailing, 1.0, -20.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        let c = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &close_button, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom, 1.0, -16.0,
        );
        c.setActive(true);
        retained_views.push(Retained::cast_unchecked(c));

        retained_views.push(Retained::cast_unchecked(content_view_retained));
        retained_views.push(Retained::cast_unchecked(separator));
        retained_views.push(Retained::cast_unchecked(close_separator));
        retained_views.push(Retained::cast_unchecked(content_container));
        retained_views.push(scroll_view);
    }

    // Stash panel widgets, delegate, sidebar, close/esc for the window's lifetime
    retained_views.push(unsafe { Retained::cast_unchecked(delegate) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_general_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_zoom_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_color_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_raw_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_file_assoc_btn) });
    retained_views.push(unsafe { Retained::cast_unchecked(sidebar_stack) });
    retained_views.push(unsafe { Retained::cast_unchecked(general.panel) });
    retained_views.push(unsafe { Retained::cast_unchecked(general.auto_update_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(general.scroll_to_zoom_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(general.scroll_to_zoom_desc) });
    retained_views.push(unsafe { Retained::cast_unchecked(general.preload_neighbors_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(general.title_bar_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(zoom.panel) });
    retained_views.push(unsafe { Retained::cast_unchecked(zoom.auto_fit_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(zoom.enlarge_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(color.panel) });
    retained_views.push(unsafe { Retained::cast_unchecked(color.icc_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(color.cm_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(color.rc_toggle) });
    retained_views.push(unsafe { Retained::cast_unchecked(raw.panel) });
    retained_views.push(unsafe { Retained::cast_unchecked(file_assoc_panel) });
    retained_views.push(unsafe { Retained::cast_unchecked(close_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(esc_button) });

    // ── Position and show ──────────────────────────────────────────────

    let parent = if parent_ns_window.is_null() {
        None
    } else {
        // SAFETY: caller guarantees this is a valid NSWindow pointer
        Some(unsafe { &*parent_ns_window })
    };
    center_window(&window, parent);

    window.makeKeyAndOrderFront(None);

    // FIXME: same leak as `show_about_window` — see the FIXME there for details.
    std::mem::forget(retained_views);
    std::mem::forget(window);

    log::debug!("Settings window shown");
}

/// Switch the Settings window to the given section by name.
/// Called by the QA server's `ShowSettingsSection` command.
pub fn switch_settings_section(section: &str) {
    let index = match section.to_lowercase().as_str() {
        "general" => 0,
        "zoom" => 1,
        "color" => 2,
        "raw" => 3,
        "file associations" | "file_associations" | "fileassociations" => 4,
        _ => {
            log::warn!("Unknown settings section: {section}");
            return;
        }
    };

    unsafe {
        let mtm = MainThreadMarker::new_unchecked();
        let app = NSApplication::sharedApplication(mtm);
        let windows: Retained<objc2_foundation::NSArray<NSWindow>> = msg_send![&*app, windows];
        let count: usize = msg_send![&*windows, count];
        let target = NSString::from_str("Settings");
        for i in 0..count {
            let win: *const NSWindow = msg_send![&*windows, objectAtIndex: i];
            if !win.is_null() {
                let win_title: Retained<NSString> = msg_send![win, title];
                let visible: bool = msg_send![win, isVisible];
                if visible && win_title.isEqualToString(&target) {
                    // Find the delegate and call select_panel
                    let delegate: *const AnyObject = msg_send![win, delegate];
                    if !delegate.is_null() {
                        let sel = match index {
                            0 => sel!(selectGeneral:),
                            1 => sel!(selectZoom:),
                            2 => sel!(selectColor:),
                            3 => sel!(selectRaw:),
                            4 => sel!(selectFileAssoc:),
                            _ => return,
                        };
                        let _: () = msg_send![delegate, performSelector: sel, withObject: std::ptr::null::<AnyObject>()];
                    }
                    return;
                }
            }
        }
    }
    log::debug!("Settings window not open, cannot switch section");
}

/// Close the Settings window if it's open.
pub fn close_settings_window() {
    unsafe {
        let mtm = MainThreadMarker::new_unchecked();
        let app = NSApplication::sharedApplication(mtm);
        let windows: Retained<objc2_foundation::NSArray<NSWindow>> = msg_send![&*app, windows];
        let count: usize = msg_send![&*windows, count];
        let target = NSString::from_str("Settings");
        for i in 0..count {
            let win: *const NSWindow = msg_send![&*windows, objectAtIndex: i];
            if !win.is_null() {
                let win_title: Retained<NSString> = msg_send![win, title];
                let visible: bool = msg_send![win, isVisible];
                if visible && win_title.isEqualToString(&target) {
                    let _: () = msg_send![win, close];
                    log::debug!("Closed settings window");
                    return;
                }
            }
        }
    }
}
