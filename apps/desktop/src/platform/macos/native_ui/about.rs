//! About window — shown via the app menu or keyboard shortcut.

use super::{
    add_vibrancy_background, as_view, center_window, is_window_already_open, load_app_icon,
    make_bold_label, make_close_button, make_escape_button, make_label, make_link,
    make_vertical_stack,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSImageScaling, NSImageView, NSLayoutAttribute,
    NSLayoutConstraint, NSLayoutRelation, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// Show the About window as a non-modal NSWindow.
///
/// Takes an optional parent NSWindow pointer to center on. The window is shown
/// immediately and returns without blocking.
///
/// # Safety
/// `parent_ns_window` must be a valid NSWindow pointer or null.
pub fn show_about_window(parent_ns_window: *const NSWindow) {
    if is_window_already_open("About Prvw") {
        return;
    }

    // SAFETY: we're on the main thread (called from winit event handler)
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let version = env!("CARGO_PKG_VERSION");

    // ── Create the window ──────────────────────────────────────────────

    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::FullSizeContentView;

    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));

    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            content_rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );

        window.setTitle(&NSString::from_str("About Prvw"));
        let _: () = msg_send![&*window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![&*window, setMovableByWindowBackground: true];

        // Prevent release on close so we don't double-free with Retained
        let _: () = msg_send![&*window, setReleasedWhenClosed: false];

        window
    };

    // ── Build content views ────────────────────────────────────────────

    // All Retained<> objects must be kept alive for the window's lifetime.
    let mut retained_views: Vec<Retained<AnyObject>> = Vec::new();

    // Add frosted glass background
    add_vibrancy_background(&window, mtm, &mut retained_views);

    // App icon
    let icon_view = {
        let icon_image = load_app_icon();
        let icon_view = NSImageView::imageViewWithImage(&icon_image, mtm);
        unsafe {
            let _: () = msg_send![
                &*icon_view,
                setImageScaling: NSImageScaling::ScaleProportionallyUpOrDown
            ];
        }

        // Constrain icon to 64x64
        let w = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &icon_view, NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute,
                1.0, 64.0,
            )
        };
        let h = unsafe {
            NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &icon_view, NSLayoutAttribute::Height,
                NSLayoutRelation::Equal,
                None::<&AnyObject>, NSLayoutAttribute::NotAnAttribute,
                1.0, 64.0,
            )
        };
        w.setActive(true);
        h.setActive(true);

        retained_views.push(unsafe { Retained::cast_unchecked(icon_image) });
        retained_views.push(unsafe { Retained::cast_unchecked(w) });
        retained_views.push(unsafe { Retained::cast_unchecked(h) });

        icon_view
    };

    let title_label = make_bold_label("Prvw", 20.0, mtm);
    let version_label = make_label(&format!("Version {version}"), 13.0, mtm);
    let subtitle_label = make_label("A fast image viewer for macOS.", 13.0, mtm);
    let author_label = make_label("By David Veszelovszki", 13.0, mtm);

    // Dim secondary text
    let secondary_color = NSColor::secondaryLabelColor();
    version_label.setTextColor(Some(&secondary_color));
    subtitle_label.setTextColor(Some(&secondary_color));
    author_label.setTextColor(Some(&secondary_color));

    let link_website = make_link(
        "veszelovszki.com",
        "https://veszelovszki.com",
        mtm,
        &mut retained_views,
    );
    let link_product = make_link(
        "getprvw.com",
        "https://getprvw.com",
        mtm,
        &mut retained_views,
    );

    let ok_button = make_close_button("Close", &window, mtm);

    // Hidden ESC button to close with Escape key
    let esc_button = make_escape_button(&window, mtm);

    // ── Layout with NSStackView ────────────────────────────────────────

    let icon_ref = unsafe { as_view::<NSImageView>(&icon_view) };
    let title_ref = unsafe { as_view::<NSTextField>(&title_label) };
    let version_ref = unsafe { as_view::<NSTextField>(&version_label) };
    let subtitle_ref = unsafe { as_view::<NSTextField>(&subtitle_label) };
    let author_ref = unsafe { as_view::<NSTextField>(&author_label) };
    let link_web_ref = unsafe { as_view::<NSTextField>(&link_website) };
    let link_prod_ref = unsafe { as_view::<NSTextField>(&link_product) };
    let button_ref = unsafe { as_view::<NSButton>(&ok_button) };

    let views: Vec<&NSView> = vec![
        icon_ref,
        title_ref,
        version_ref,
        subtitle_ref,
        author_ref,
        link_web_ref,
        link_prod_ref,
        button_ref,
    ];

    let stack = make_vertical_stack(&views, 6.0, mtm);

    // Extra spacing after the icon and before the OK button
    stack.setCustomSpacing_afterView(12.0, icon_ref);
    stack.setCustomSpacing_afterView(16.0, link_prod_ref);

    // Set the stack as the window's content view with padding
    unsafe {
        let _: () = msg_send![
            &*stack,
            setTranslatesAutoresizingMaskIntoConstraints: false
        ];

        let content_view: *mut NSView = msg_send![&*window, contentView];
        let content_view_ref = &*content_view;
        let content_view_retained = Retained::retain(content_view).unwrap();

        content_view_ref.addSubview(&stack);

        // Add the hidden ESC button to the content view (not the stack)
        content_view_ref.addSubview(as_view::<NSButton>(&esc_button));

        // Pin stack edges to content view with padding.
        // Top is larger to clear the transparent titlebar.
        let top = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Top,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Top,
            1.0, 36.0,
        );
        let bottom = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::Bottom,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::Bottom,
            1.0, -20.0,
        );
        let cx = NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &stack, NSLayoutAttribute::CenterX,
            NSLayoutRelation::Equal,
            Some(content_view_ref as &AnyObject), NSLayoutAttribute::CenterX,
            1.0, 0.0,
        );

        top.setActive(true);
        bottom.setActive(true);
        cx.setActive(true);

        retained_views.push(Retained::cast_unchecked(top));
        retained_views.push(Retained::cast_unchecked(bottom));
        retained_views.push(Retained::cast_unchecked(cx));
        retained_views.push(Retained::cast_unchecked(content_view_retained));
    }

    // Store all Retained objects so they live as long as the window
    retained_views.push(unsafe { Retained::cast_unchecked(icon_view) });
    retained_views.push(unsafe { Retained::cast_unchecked(title_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(version_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(subtitle_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(author_label) });
    retained_views.push(unsafe { Retained::cast_unchecked(link_website) });
    retained_views.push(unsafe { Retained::cast_unchecked(link_product) });
    retained_views.push(unsafe { Retained::cast_unchecked(ok_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(esc_button) });
    retained_views.push(unsafe { Retained::cast_unchecked(stack) });

    // ── Position and show ──────────────────────────────────────────────

    let parent = if parent_ns_window.is_null() {
        None
    } else {
        // SAFETY: caller guarantees this is a valid NSWindow pointer
        Some(unsafe { &*parent_ns_window })
    };
    center_window(&window, parent);

    window.makeKeyAndOrderFront(None);

    // FIXME: leaks ~a few KB per window open. Each call to `show_about_window` leaks the
    // Retained<> wrappers and the window itself. The deduplication guard above prevents
    // stacking, but if the user closes and re-opens, it leaks again. Proper fix: use an
    // NSWindowDelegate with `windowWillClose:` to clean up, or store in a static Option.
    std::mem::forget(retained_views);
    std::mem::forget(window);

    log::debug!("About window shown");
}
