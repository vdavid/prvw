//! The one message hook winit allows, and the ordered stages that share it.
//!
//! ## Why there is a hook at all
//!
//! Two Win32 things have to happen between taking a message off the queue and dispatching it,
//! and neither winit nor muda does them:
//!
//! 1. **`IsDialogMessageW`**, for a modeless dialog. Without it Tab, the arrow keys, Enter, Esc,
//!    and mnemonics all stop working inside the dialog. `about::windows` is the first caller and
//!    M4's settings window is the next.
//! 2. **`TranslateAcceleratorW`**, for the menu bar's accelerators. muda's own docs say so:
//!    "On Windows, accelerators don't work unless the win32 message loop calls
//!    `TranslateAcceleratorW`". Without it Ctrl+O, Ctrl+= and Ctrl+-, F11, and F5 silently do
//!    nothing.
//!
//! `EventLoopBuilderExtWindows::with_msg_hook` is the injection point, and winit stores exactly
//! **one** hook, so both live in one closure and the order between them is a decision rather
//! than an accident. That is what this module is: [`install`] is called once, and each stage
//! registers what it owns.
//!
//! ## The order, and why it is this way
//!
//! Dialogs first. A message `IsDialogMessage` handles must not also reach accelerator
//! translation, which is why stage 1 returns rather than falling through.
//!
//! Then accelerators, **against the main window's HWND rather than `msg.hwnd`**. muda's own
//! winit example passes `(*msg).hwnd`, which translates accelerators against whatever window has
//! focus: typing a comma into a settings text field would open Settings. Passing the main window
//! means an accelerator always means the same thing, and stage 1 has already taken the dialogs'
//! own messages off the table.
//!
//! ## The rule this module exists to keep
//!
//! **No nested message loop, ever.** It is the Windows form of the macOS rule about AppKit
//! modals inside winit's callbacks (`AGENTS.md`), with a different failure: a Win32 modal loop
//! doesn't crash, it starves winit's pump. `about_to_wait` stops running, so `ControlFlow::
//! WaitUntil` timers and `EventLoopProxy` user events stall, and the slideshow freezes. So
//! `DialogBoxParam`, `TaskDialogIndirect`, and `IFileDialog::Show` are all out;
//! `CreateDialogParamW` plus this hook is the shape that works.
//!
//! The one exception is the loops Windows itself owns: a menu drop-down (`WM_ENTERMENULOOP`) and
//! a title-bar drag (`WM_ENTERSIZEMOVE`). Every Win32 app has those, and the slideshow timer
//! pausing while a menu is open is native behavior rather than a bug.

use std::cell::RefCell;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    HACCEL, IsChild, IsDialogMessageW, MSG, TranslateAcceleratorW,
};
use winit::event_loop::EventLoopBuilder;
use winit::platform::windows::EventLoopBuilderExtWindows;

use crate::commands::AppCommand;

/// Where accelerator keystrokes go, and the table that translates them.
pub struct AcceleratorTarget {
    /// The window a translated accelerator posts its `WM_COMMAND` to: the main window, always.
    pub hwnd: HWND,
    /// muda's accelerator table for the menu bar. Read per message, not cached: muda destroys
    /// and recreates it whenever an item joins or leaves a menu.
    pub haccel: HACCEL,
}

/// What the stages know. All of it is thread-local because the event loop's thread is the only
/// one that pumps messages, and the only one allowed to touch an `HWND` of ours.
#[derive(Default)]
struct Stages {
    /// Open modeless dialogs. A set rather than a stack: modeless dialogs are concurrent, not
    /// nested, so there is no innermost one to prefer.
    dialogs: Vec<HWND>,
    /// Whoever owns the menu bar answers this. A plain function pointer, so the hook doesn't
    /// have to hold a muda handle of its own.
    accelerators: Option<fn() -> Option<AcceleratorTarget>>,
}

thread_local! {
    static STAGES: RefCell<Stages> = RefCell::new(Stages::default());
}

/// Install the hook on the event loop being built. Call once, before `build()`.
pub fn install(builder: &mut EventLoopBuilder<AppCommand>) {
    builder.with_msg_hook(|msg| {
        // SAFETY: winit calls this right after `PeekMessageW(.., PM_REMOVE)` and documents the
        // pointer as the `MSG` it just took off the queue, which outlives this call. `hwnd` is
        // 0 in the hook's own registration, not here.
        handle(msg.cast::<MSG>())
    });
    log::debug!("Win32 message hook installed");
}

/// Returning true tells winit to skip its own `TranslateMessage` / `DispatchMessageW` for this
/// message, which is what stops a translated accelerator from also arriving as a keystroke.
fn handle(msg: *const MSG) -> bool {
    // SAFETY: winit guarantees a live `MSG` for the duration of the hook.
    let target = unsafe { (*msg).hwnd };

    // 1. Modeless dialogs. `IsDialogMessage` respects a control that claims a key with
    //    `DLGC_WANTALLKEYS`, which is most of why it beats hand-rolled Tab handling.
    for dialog in dialogs() {
        // SAFETY: both handles are windows this thread owns; a stale one answers false.
        if dialog == target || unsafe { IsChild(dialog, target) }.as_bool() {
            // SAFETY: the dialog is live and `msg` is winit's.
            return unsafe { IsDialogMessageW(dialog, msg) }.as_bool();
        }
    }

    // 2. Menu accelerators, against the main window (see the module docs).
    let Some(accelerators) = accelerator_target() else {
        return false;
    };
    // SAFETY: the window and the accelerator table both belong to this thread, and `haccel` was
    // read from muda a moment ago rather than cached across menu edits.
    unsafe { TranslateAcceleratorW(accelerators.hwnd, accelerators.haccel, msg) != 0 }
}

/// A snapshot, so no `RefCell` borrow is held across a call into Win32: `IsDialogMessageW`
/// dispatches to a window procedure, and a window procedure can reach back in here.
fn dialogs() -> Vec<HWND> {
    STAGES.with_borrow(|stages| stages.dialogs.clone())
}

fn accelerator_target() -> Option<AcceleratorTarget> {
    let source = STAGES.with_borrow(|stages| stages.accelerators)?;
    source()
}

/// Point stage 2 at the menu bar's accelerator table. `menu::windows::attach` calls this once
/// the bar is on the window.
pub fn set_accelerator_source(source: fn() -> Option<AcceleratorTarget>) {
    STAGES.with_borrow_mut(|stages| stages.accelerators = Some(source));
}

/// Add a modeless dialog to stage 1, so its keyboard navigation works.
///
/// `about::windows` is the first caller, and M4's settings window is the next. Its order against
/// accelerator translation is the part that's easy to get wrong once there are two callers and
/// only one hook, which is why the stage was built before either of them existed.
pub fn register_dialog(hwnd: HWND) {
    STAGES.with_borrow_mut(|stages| {
        if !stages.dialogs.contains(&hwnd) {
            stages.dialogs.push(hwnd);
        }
    });
}

/// Take a dialog back out of stage 1, when it closes. Every caller does this from `WM_DESTROY`.
pub fn unregister_dialog(hwnd: HWND) {
    STAGES.with_borrow_mut(|stages| stages.dialogs.retain(|open| *open != hwnd));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hwnd(value: usize) -> HWND {
        HWND(value as *mut std::ffi::c_void)
    }

    /// A dialog registers once however many times it is announced, and closing takes it out.
    /// A double entry would run `IsDialogMessageW` twice for the same message.
    #[test]
    fn dialogs_are_a_set() {
        register_dialog(hwnd(1));
        register_dialog(hwnd(1));
        register_dialog(hwnd(2));
        assert_eq!(dialogs(), vec![hwnd(1), hwnd(2)]);

        unregister_dialog(hwnd(1));
        assert_eq!(dialogs(), vec![hwnd(2)]);
        unregister_dialog(hwnd(2));
        assert!(dialogs().is_empty());
    }

    /// With no menu bar attached there is nothing to translate against, and the hook has to
    /// leave the message to winit rather than guess a target.
    #[test]
    fn no_menu_bar_means_no_accelerator_target() {
        assert!(accelerator_target().is_none());
    }
}
