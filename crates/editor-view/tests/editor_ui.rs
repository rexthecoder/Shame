//! UI tests for `EditorView`: a real editor in a headless GPUI window,
//! driven by simulated key presses and clicks, checked through the buffer
//! and the rendered element tree.
//!
//! Run with `cargo test -p editor-view --test editor_ui`.

use core_buffer::Buffer;
use editor_view::EditorView;
use editor_view::keymap::Keymap;
use gpui::{
    Entity, Focusable as _, Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent,
    TestAppContext, VisualTestContext, point, px,
};

/// Open `text` in a focused editor (no file path, so no language server).
fn open_editor<'a>(cx: &'a mut TestAppContext, text: &str) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let text = text.to_string();
    let (editor, cx) = cx.add_window_view(move |_window, cx| EditorView::from_buffer(Buffer::from_str(&text), cx));
    cx.update(|window, cx| {
        let handle = editor.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    });
    cx.run_until_parked();
    (editor, cx)
}

fn text(editor: &Entity<EditorView>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| editor.read(cx).buffer().text())
}

#[gpui::test]
fn typing_inserts_text_but_unbound_shortcuts_do_not(cx: &mut TestAppContext) {
    let (editor, cx) = open_editor(cx, "");
    cx.simulate_keystrokes("h i");
    assert_eq!(text(&editor, cx), "hi");

    // ⌘K isn't bound by default: it must not type a "k".
    cx.simulate_keystrokes("cmd-k");
    assert_eq!(text(&editor, cx), "hi");
}

#[gpui::test]
fn select_all_delete_and_undo(cx: &mut TestAppContext) {
    let (editor, cx) = open_editor(cx, "void main() {}\n");
    cx.simulate_keystrokes("cmd-a backspace");
    assert_eq!(text(&editor, cx), "");

    cx.simulate_keystrokes("cmd-z");
    assert_eq!(text(&editor, cx), "void main() {}\n");
}

#[gpui::test]
fn ctrl_triggers_cmd_bindings(cx: &mut TestAppContext) {
    let (editor, cx) = open_editor(cx, "abc");
    cx.simulate_keystrokes("ctrl-a backspace");
    assert_eq!(text(&editor, cx), "");
}

#[gpui::test]
fn user_keymap_rebinds_and_unbinds(cx: &mut TestAppContext) {
    let (editor, cx) = open_editor(cx, "abc");
    cx.update(|_, cx| cx.set_global(Keymap::with_overrides(r#"{ "cmd-k": "select-all", "cmd-a": null }"#)));

    // ⌘A no longer selects everything (and doesn't type either).
    cx.simulate_keystrokes("cmd-a backspace");
    assert_eq!(text(&editor, cx), "abc", "backspace at the start of the buffer is a no-op");

    cx.simulate_keystrokes("cmd-k backspace");
    assert_eq!(text(&editor, cx), "");
}

#[gpui::test]
fn code_action_menu_opens_at_the_cursor_and_closes(cx: &mut TestAppContext) {
    let (editor, cx) = open_editor(cx, "final x = 1;\n");
    assert!(cx.debug_bounds("code-action-menu").is_none());

    // No language server here, so the menu says there's nothing to offer
    // rather than silently doing nothing.
    cx.simulate_keystrokes("cmd-.");
    assert!(cx.debug_bounds("code-action-menu").is_some(), "⌘. opens the menu");

    cx.simulate_keystrokes("escape");
    assert!(cx.debug_bounds("code-action-menu").is_none(), "Esc closes it");

    // Any other key closes the menu and still does its normal job.
    cx.simulate_keystrokes("cmd-. z");
    assert!(cx.debug_bounds("code-action-menu").is_none());
    assert_eq!(text(&editor, cx), "zfinal x = 1;\n");
}

#[gpui::test]
fn code_action_menu_sits_below_the_cursor_line(cx: &mut TestAppContext) {
    let (_editor, cx) = open_editor(cx, "one\ntwo\nthree\n");
    cx.simulate_keystrokes("down cmd-.");
    let line = cx.debug_bounds("editor-line-1").expect("line 1 is rendered");
    let menu = cx.debug_bounds("code-action-menu").expect("menu is open");
    assert!(menu.origin.y >= line.origin.y + line.size.height, "menu {menu:?} should start below line {line:?}");
}

#[gpui::test]
fn clicking_a_line_moves_the_cursor_there(cx: &mut TestAppContext) {
    let (editor, cx) = open_editor(cx, "first\nsecond\nthird\n");
    let line = cx.debug_bounds("editor-line-2").expect("line 2 is rendered");
    cx.simulate_click(line.center(), Modifiers::none());
    // `cursor_position` is 1-based ("Ln 3"), like the status bar shows it.
    let (row, _col) = cx.update(|_, cx| editor.read(cx).cursor_position());
    assert_eq!(row, 3);
}

#[gpui::test]
fn a_long_line_scrolls_horizontally(fx: &mut TestAppContext) {
    let long = "x".repeat(400);
    let (editor, cx) = open_editor(fx, &format!("{long}\nshort\n"));
    let first = cx.debug_bounds("editor-line-0").expect("line 0 is rendered");
    assert!(
        cx.debug_bounds("editor-h-scrollbar").is_some(),
        "the bar appears when a line is wider than the pane"
    );

    let at = point(px(500.), first.center().y);
    cx.simulate_click(at, Modifiers::none());
    let (_, before) = cx.update(|_, cx| editor.read(cx).cursor_position());

    cx.simulate_event(ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(-300.), px(0.))),
        ..Default::default()
    });
    cx.run_until_parked();

    cx.simulate_click(at, Modifiers::none());
    let (_, after) = cx.update(|_, cx| editor.read(cx).cursor_position());
    assert!(
        after > before + 20,
        "the same x maps further along the line after panning: {before} -> {after}"
    );
}

#[gpui::test]
fn dragging_the_horizontal_bar_pans_the_text(cx: &mut TestAppContext) {
    let long = "x".repeat(400);
    let (editor, cx) = open_editor(cx, &long);
    let thumb = cx.debug_bounds("editor-h-thumb").expect("the scrollbar thumb");
    let start = thumb.center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    let moved = point(start.x + px(40.), start.y);
    cx.simulate_mouse_move(moved, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_up(moved, MouseButton::Left, Modifiers::none());

    let scrolled = cx.update(|_, cx| editor.read(cx).horizontal_scroll());
    assert!(scrolled > 10., "dragging the thumb pans the code, got {scrolled}");
}

#[gpui::test]
fn a_short_file_has_no_horizontal_bar(cx: &mut TestAppContext) {
    let (_editor, cx) = open_editor(cx, "void main() {}\n");
    assert!(cx.debug_bounds("editor-h-scrollbar").is_none());
}

#[gpui::test]
fn the_view_follows_the_cursor_to_the_end_of_a_long_line(cx: &mut TestAppContext) {
    let long = "y".repeat(400);
    let (editor, cx) = open_editor(cx, &long);
    cx.simulate_keystrokes("end");

    // End put the caret at the far right, so the pane has panned there: a
    // click near the right edge must land near the end of the line, not in
    // the middle of the text that used to be visible there.
    let line = cx.debug_bounds("editor-line-0").expect("line 0 is rendered");
    cx.simulate_click(point(px(1800.), line.center().y), Modifiers::none());
    let (_, column) = cx.update(|_, cx| editor.read(cx).cursor_position());
    assert!(column > 300, "the pane followed the cursor, got column {column}");
}

#[gpui::test]
fn search_toggles_narrow_the_matches(cx: &mut TestAppContext) {
    let (editor, cx) = open_editor(cx, "Foo foo food\n");
    let status = |cx: &mut VisualTestContext| cx.update(|_, cx| editor.read(cx).search_status().1);

    cx.update(|_, cx| editor.update(cx, |e, cx| e.set_search_query("foo", cx)));
    assert_eq!(status(cx), 3, "case-insensitive by default");

    cx.update(|_, cx| editor.update(cx, |e, cx| e.set_search_options(true, false, cx)));
    assert_eq!(status(cx), 2, "match case drops `Foo`");

    cx.update(|_, cx| editor.update(cx, |e, cx| e.set_search_options(true, true, cx)));
    assert_eq!(status(cx), 1, "whole word also drops `food`");
}
