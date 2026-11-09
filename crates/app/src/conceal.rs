//! Keeps a hidden macOS instance off the person's screen: macOS has no private display like Xvfb,
//! so the app never activates and its windows stay transparent and click-through while GPUI keeps
//! drawing them.

use gpui::{App, Window, WindowId};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSView, NSWindowCollectionBehavior};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::collections::HashSet;
use std::time::Duration;

pub fn start(cx: &mut App) {
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    NSApplication::sharedApplication(main_thread).setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    cx.spawn(async move |cx| {
        let mut concealed = HashSet::<WindowId>::new();
        loop {
            cx.update(|cx| {
                for handle in cx.windows() {
                    let id = handle.window_id();
                    if !concealed.contains(&id) && handle.update(cx, |_, window, _| conceal(window)).unwrap_or(false) {
                        concealed.insert(id);
                    }
                }
            });
            cx.background_executor().timer(Duration::from_millis(20)).await;
        }
    })
    .detach();
}

fn conceal(window: &Window) -> bool {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return false;
    };
    let view: &NSView = unsafe { appkit.ns_view.cast().as_ref() };
    let Some(ns_window) = view.window() else {
        return false;
    };
    ns_window.setAlphaValue(0.0);
    ns_window.setIgnoresMouseEvents(true);
    ns_window.setHasShadow(false);
    ns_window.setExcludedFromWindowsMenu(true);
    ns_window.setCollectionBehavior(NSWindowCollectionBehavior::Transient | NSWindowCollectionBehavior::IgnoresCycle);
    true
}
