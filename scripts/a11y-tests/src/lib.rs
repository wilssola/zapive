#[cfg(test)]
mod tests {
    use slint::{ComponentHandle, platform::Key, platform::WindowEvent};
    use std::time::Duration;

    slint::slint! {
        import { AccessibleList, AccessibleListItem, KeyFocus } from "../../../ui/components/a11y.slint";
        export component Harness inherits Window {
            width: 320px;
            height: 240px;
            in-out property <int> count: 60;
            in-out property <bool> variable;
            out property <length> offset: list.content-y;
            out property <length> extent: list.content-height;
            out property <int> active: list.active-index;
            out property <int> pending: list.pending-index;
            out property <bool> seeking: list.seeking;
            out property <bool> before-focused: before.has-focus;
            out property <bool> after-focused: after.has-focus;
            in-out property <int> focused-row: -1;
            in-out property <int> focused-control: -1;
            in-out property <int> activations;
            in-out property <int> control-activations;
            in-out property <int> live-rows;
            in-out property <length> focused-top;
            in-out property <length> focused-height;
            public function request(index: int) { list.request-row(index); }
            public function focus-before() { before.focus(); }
            public function focus-after() { after.focus(); }
            before := FocusScope { y: 0px; height: 20px; }
            list := AccessibleList {
                y: 20px;
                width: parent.width;
                height: 180px;
                row-count: root.count;
                fixed-row-height: root.variable ? 0px : 30px;
                for data[i] in root.count: row := AccessibleListItem {
                    init => { root.live-rows += 1; }
                    row-index: i;
                    row-count: root.count;
                    row-id: "" + i;
                    active-index: list.active-index;
                    active-id: list.active-id;
                    pending-index: list.pending-index;
                    focus-pass: list.focus-pass;
                    probe-pass: list.probe-pass;
                    scroll-y: list.content-y;
                    height: root.variable ? (i == 23 ? 260px : 24px + mod(i, 5) * 17px) : 30px;
                    has-controls: true;
                    navigate(index) => { list.request-row(index); }
                    focused(index, id) => {
                        root.focused-row = index;
                        list.row-focused(index, id);
                    }
                    geometry(index, top, size) => {
                        if (index == list.active-index) {
                            root.focused-top = top;
                            root.focused-height = size;
                        }
                        list.report-row(index, top, size);
                    }
                    activate => { root.activations += 1; }
                    enter-controls => { self.focus-controls(); }
                    controls-changed(active) => { list.controls-active = active; }
                    leave-list(backwards) => {
                        if (backwards) { before.focus(); }
                        else { after.focus(); }
                    }
                    KeyFocus {
                        focus-on-tab-navigation: row.controls-mode;
                        focus-gained => { root.focused-control = i; }
                        activate => { root.control-activations += 1; }
                    }
                }
            }
            after := FocusScope { y: 210px; height: 20px; }
            Timer {
                interval: 16ms;
                running: list.seeking;
                triggered => { list.seek-step(); }
            }
        }
    }

    fn settle() {
        for _ in 0..160 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        }
    }

    fn key(ui: &Harness, key: Key) {
        let text: slint::SharedString = key.into();
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
    }

    fn shift_tab(ui: &Harness) {
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Shift.into(),
        });
        key(ui, Key::Tab);
        ui.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Shift.into(),
        });
        settle();
    }

    fn harness(variable: bool) -> Harness {
        i_slint_backend_testing::init_no_event_loop();
        let ui = Harness::new().unwrap();
        ui.set_variable(variable);
        ui.show().unwrap();
        settle();
        ui
    }

    #[test]
    fn keyboard_navigation_virtualized_fixed_rows() {
        let ui = harness(false);
        assert!(ui.get_live_rows() < ui.get_count());
        ui.invoke_focus_before();
        key(&ui, Key::Tab);
        settle();
        assert_eq!(ui.get_focused_row(), 0);
        key(&ui, Key::DownArrow);
        settle();
        assert_eq!(ui.get_focused_row(), 1);
        key(&ui, Key::End);
        settle();
        assert_eq!(ui.get_focused_row(), 59);
        key(&ui, Key::Home);
        settle();
        assert_eq!(ui.get_focused_row(), 0);
        ui.invoke_request(37);
        settle();
        assert_eq!(ui.get_focused_row(), 37);
        assert!(ui.get_live_rows() < ui.get_count());
        ui.invoke_request(37);
        settle();
        assert!(!ui.get_seeking());
        assert_eq!(ui.get_pending(), -1);
        key(&ui, Key::Return);
        assert_eq!(ui.get_activations(), 1);
        key(&ui, Key::Tab);
        settle();
        assert!(ui.get_after_focused());
        shift_tab(&ui);
        assert_eq!(ui.get_focused_row(), 37);
        shift_tab(&ui);
        assert!(ui.get_before_focused());
    }

    #[test]
    fn variable_rows_seek_and_controls_are_isolated() {
        let ui = harness(true);
        for index in [0, 59, 23, 45, 2, 37, 37] {
            ui.invoke_request(index);
            settle();
            assert_eq!(
                ui.get_focused_row(),
                index,
                "offset {} extent {} pending {} seeking {}",
                ui.get_offset(),
                ui.get_extent(),
                ui.get_pending(),
                ui.get_seeking()
            );
            assert!(!ui.get_seeking(), "seek did not finish for {index}");
            let previous = ui.get_activations();
            key(&ui, Key::Return);
            assert_eq!(
                ui.get_activations(),
                previous + 1,
                "row {index} lost keyboard focus"
            );
        }
        assert_eq!(ui.get_activations(), 7);
        key(&ui, Key::Home);
        settle();
        assert_eq!(ui.get_active(), 0);
        key(&ui, Key::End);
        settle();
        assert_eq!(ui.get_active(), 59);
        ui.invoke_request(37);
        settle();
        key(&ui, Key::F2);
        settle();
        key(&ui, Key::Tab);
        settle();
        assert_eq!(ui.get_focused_control(), 37);
        key(&ui, Key::Return);
        assert_eq!(ui.get_control_activations(), 1);
        assert_eq!(ui.get_activations(), 7);
        key(&ui, Key::DownArrow);
        settle();
        assert_eq!(ui.get_active(), 37);
        shift_tab(&ui);
        assert!(ui.get_before_focused());
        ui.invoke_request(37);
        settle();
        key(&ui, Key::F2);
        settle();
        key(&ui, Key::Tab);
        settle();
        key(&ui, Key::Escape);
        settle();
        key(&ui, Key::DownArrow);
        settle();
        assert_eq!(ui.get_active(), 38);
        key(&ui, Key::F2);
        settle();
        key(&ui, Key::Tab);
        settle();
        assert_eq!(ui.get_focused_control(), 38);
        key(&ui, Key::Tab);
        settle();
        assert!(ui.get_after_focused());
    }

    #[test]
    fn empty_and_clamped_requests() {
        let ui = harness(false);
        ui.invoke_request(-10);
        settle();
        assert_eq!(ui.get_active(), 0);
        ui.invoke_request(1000);
        settle();
        assert_eq!(ui.get_active(), 59);
        ui.set_count(0);
        settle();
        ui.invoke_request(20);
        settle();
        assert!(!ui.get_seeking());
        assert_eq!(ui.get_pending(), -1);
    }

    #[test]
    fn leaving_cancels_pending_focus() {
        let ui = harness(true);
        ui.invoke_request(0);
        settle();
        ui.invoke_request(45);
        ui.invoke_focus_after();
        settle();
        assert!(ui.get_after_focused());
        assert!(!ui.get_seeking());
        assert_eq!(ui.get_pending(), -1);
    }
}
