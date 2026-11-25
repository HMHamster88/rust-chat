//! Event wiring for every control the views create.

use super::*;

pub(super) fn wire(ui: &Rc<Ui>) {
    wire_tooltips(ui);
    ui.connect_button.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.connect();
            }
        }
    });
    ui.forget.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.forget_key();
            }
        }
    });
    ui.remember.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                // Remembering a password must survive a restart on its own.
                ui.save_connection_form();
            }
        }
    });
    ui.approve.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.client.trust(true);
            }
        }
    });
    ui.deny.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.client.trust(false);
            }
        }
    });
    ui.send.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.send_message();
            }
        }
    });
    ui.disconnect.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.leave_voice(Some("voice: disconnected"));
                let _ = ui.client.disconnect();
            }
        }
    });
    ui.join_voice.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.join_voice();
            }
        }
    });
    ui.leave_voice.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.leave_voice(None);
            }
        }
    });
    ui.mute_mic.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.toggle_mic();
            }
        }
    });
    ui.mute_speaker.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.toggle_speaker();
            }
        }
    });
    ui.share.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.toggle_share();
            }
        }
    });
    ui.share_source.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_share_source();
            }
        }
    });
    ui.share_quality.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_share_quality();
            }
        }
    });
    ui.request_control.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.request_control();
            }
        }
    });
    ui.stop_control.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.stop_control();
            }
        }
    });
    ui.allow_control.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.respond_control(true);
            }
        }
    });
    ui.deny_control.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.respond_control(false);
            }
        }
    });
    ui.file_choose.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.choose_file();
            }
        }
    });
    ui.file_send.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.send_file();
            }
        }
    });
    ui.file_accept.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.accept_offer();
            }
        }
    });
    ui.file_decline.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.decline_offer();
            }
        }
    });
    ui.file_cancel.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cancel_file();
            }
        }
    });
    ui.file_open.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.open_last_file();
            }
        }
    });
    ui.update_check.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.check_updates();
            }
        }
    });
    ui.update_open.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.open_release();
            }
        }
    });
    ui.update_auto.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.set_update_auto(ui.update_auto.is_checked().unwrap_or(true));
            }
        }
    });

    let screen = &ui.settings_screen;
    for button in [screen.open_room.clone(), screen.open_connect.clone()] {
        button.click({
            let weak = Rc::downgrade(ui);
            move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.set_settings_visible(true);
                }
            }
        });
    }
    for (index, button) in screen.nav.iter().enumerate() {
        button.click({
            let weak = Rc::downgrade(ui);
            move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.select_settings_section(index);
                }
            }
        });
    }
    screen.close.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cancel_confirm();
                ui.set_settings_visible(false);
            }
        }
    });
    screen.auto_connect.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui
                    .settings_screen
                    .auto_connect
                    .is_checked()
                    .unwrap_or(false);
                ui.change_settings(|settings| settings.auto_connect = checked);
            }
        }
    });
    screen.timestamps.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui.settings_screen.timestamps.is_checked().unwrap_or(true);
                ui.change_settings(|settings| settings.show_timestamps = checked);
            }
        }
    });
    screen.clock24.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui.settings_screen.clock24.is_checked().unwrap_or(true);
                ui.change_settings(|settings| settings.clock_24h = checked);
            }
        }
    });
    screen.keep_history.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui.settings_screen.keep_history.is_checked().unwrap_or(true);
                ui.change_settings(|settings| settings.keep_history = checked);
            }
        }
    });
    screen.open_when_finished.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let checked = ui
                    .settings_screen
                    .open_when_finished
                    .is_checked()
                    .unwrap_or(false);
                ui.change_settings(|settings| settings.open_when_finished = checked);
            }
        }
    });
    screen.mic.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_device(true);
            }
        }
    });
    screen.speaker.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_device(false);
            }
        }
    });
    screen.quality.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cycle_settings_quality();
            }
        }
    });
    screen.choose_downloads.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.choose_downloads();
            }
        }
    });
    screen.reset_downloads.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.reset_downloads();
            }
        }
    });
    screen.forget_pin.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.arm_confirm(ConfirmAction::ForgetPin);
            }
        }
    });
    screen.clear_history.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.arm_confirm(ConfirmAction::ClearHistory);
            }
        }
    });
    screen.confirm.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.run_confirm();
            }
        }
    });
    screen.cancel.click({
        let weak = Rc::downgrade(ui);
        move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cancel_confirm();
            }
        }
    });
}

/// Attaches the hover tooltip to every icon button created by `icons::icon_button`.
fn wire_tooltips(ui: &Rc<Ui>) {
    let mut tokens = Vec::new();
    for (button, label) in icons::take_buttons() {
        tokens.push(button.add_handler(InputElement::pointer_entered_event(), {
            let weak = Rc::downgrade(ui);
            let layer = ui.tip_layer.clone();
            move |_, e| {
                if let Some(ui) = weak.upgrade() {
                    let at = e.get_position(Some(&layer));
                    ui.show_tip(&label, at);
                }
            }
        }));
        tokens.push(button.add_handler(InputElement::pointer_exited_event(), {
            let weak = Rc::downgrade(ui);
            move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.hide_tip();
                }
            }
        }));
    }
    ui.input_tokens.borrow_mut().extend(tokens);
}

pub(super) fn attach_control_input(ui: &Rc<Ui>) {
    ui.presenter.set_focusable(true);
    ui.presenter.set_is_tab_stop(true);
    let mut tokens = Vec::new();
    tokens.push(
        ui.video_host
            .add_handler(InputElement::pointer_moved_event(), {
                let weak = Rc::downgrade(ui);
                move |_, e| {
                    if let Some(ui) = weak.upgrade() {
                        ui.on_pointer_moved(e);
                    }
                }
            }),
    );
    tokens.push(
        ui.video_host
            .add_handler(InputElement::pointer_pressed_event(), {
                let weak = Rc::downgrade(ui);
                move |_, e| {
                    if let Some(ui) = weak.upgrade() {
                        ui.on_pointer_pressed(e);
                    }
                }
            }),
    );
    tokens.push(
        ui.video_host
            .add_handler(InputElement::pointer_released_event(), {
                let weak = Rc::downgrade(ui);
                move |_, e| {
                    if let Some(ui) = weak.upgrade() {
                        ui.on_pointer_released(e);
                    }
                }
            }),
    );
    tokens.push(
        ui.video_host
            .add_handler(InputElement::pointer_wheel_changed_event(), {
                let weak = Rc::downgrade(ui);
                move |_, e| {
                    if let Some(ui) = weak.upgrade() {
                        ui.on_pointer_wheel(e);
                    }
                }
            }),
    );
    tokens.push(ui.presenter.add_handler(InputElement::key_down_event(), {
        let weak = Rc::downgrade(ui);
        move |_, e| {
            if let Some(ui) = weak.upgrade() {
                ui.on_key(e, true);
            }
        }
    }));
    tokens.push(ui.presenter.add_handler(InputElement::key_up_event(), {
        let weak = Rc::downgrade(ui);
        move |_, e| {
            if let Some(ui) = weak.upgrade() {
                ui.on_key(e, false);
            }
        }
    }));
    ui.input_tokens.borrow_mut().extend(tokens);
}
