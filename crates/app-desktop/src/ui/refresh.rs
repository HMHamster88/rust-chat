//! The 100 ms refresh: connection state, rooms, members, messages, voice and files.

use super::*;

pub(super) fn smoke_request(settings: &ClientSettings) -> Option<ConnectRequest> {
    let host = std::env::var("RUST_CHAT_SMOKE_HOST").ok()?;
    let port = std::env::var("RUST_CHAT_SMOKE_PORT").ok()?.parse().ok()?;
    let password = std::env::var("RUST_CHAT_SMOKE_PASSWORD").ok()?;
    if host.is_empty() || password.is_empty() {
        return None;
    }
    let name = std::env::var("RUST_CHAT_SMOKE_NAME").unwrap_or_else(|_| {
        if settings.name.is_empty() {
            "smoke".to_owned()
        } else {
            settings.name.clone()
        }
    });
    let tls = std::env::var("RUST_CHAT_SMOKE_TLS")
        .map(|value| value != "0")
        .unwrap_or(false);
    Some(ConnectRequest {
        host,
        port,
        tls,
        name,
        password,
        room: None,
    })
}

pub(super) fn refresh(ui: &Rc<Ui>) {
    let snapshot = ui.client.snapshot();
    let room_visible = snapshot.room.is_some();
    let settings_open = ui.settings_screen.visible.get();
    ui.room_view.set_is_visible(room_visible && !settings_open);
    ui.connect_view
        .set_is_visible(!room_visible && !settings_open);
    ui.settings_screen.view.set_is_visible(settings_open);

    let mut status = snapshot.connection.label();
    if let ConnectionState::Failed(failure) = &snapshot.connection
        && let Some(hint) = &failure.hint
    {
        status.push('\n');
        status.push_str(hint);
    }
    ui.connect_status.set_text(Some(&status));
    ui.error_text
        .set_text(Some(snapshot.last_error.as_deref().unwrap_or("")));

    let update_result = ui
        .update_rx
        .borrow()
        .as_ref()
        .and_then(|rx| rx.try_recv().ok());
    if let Some(result) = update_result {
        match result {
            Ok(Some(release)) => {
                ui.update_status
                    .set_text(Some(&format!("version {} is available", release.version)));
                *ui.update_url.borrow_mut() = Some(release.url);
                ui.update_open.set_is_visible(true);
            }
            Ok(None) => ui
                .update_status
                .set_text(Some(&format!("up to date (v{APP_VERSION})"))),
            Err(error) => ui.update_status.set_text(Some(&error)),
        }
    }

    if let Some(trust) = &snapshot.trust {
        ui.trust_panel.set_is_visible(true);
        let text = format!(
            "Server key\n{}\nApprove only if it matches the fingerprint the server printed.",
            trust.fingerprint
        );
        ui.trust_text.set_text(Some(&text));
    } else {
        ui.trust_panel.set_is_visible(false);
    }

    if snapshot.room.is_none() {
        ui.leave_voice(Some("off"));
        ui.join_voice.set_is_visible(false);
        ui.leave_voice.set_is_visible(false);
        ui.mute_mic.set_is_visible(false);
        ui.mute_speaker.set_is_visible(false);
        ui.voice_status.set_text(Some(""));
    }

    if let Some(room) = &snapshot.room {
        refresh_voice(ui, &snapshot);
        let presenting = room
            .members
            .iter()
            .any(|member| member.sharing && member.session_id != room.self_member.session_id);
        render_video(ui, presenting);
        ui.stage_frame.set_is_visible(presenting);
        let title = format!(
            "# {}   ·   {} member{}",
            room.room.name,
            room.members.len(),
            if room.members.len() == 1 { "" } else { "s" }
        );
        ui.room_title.set_text(Some(&title));
        let connection = match snapshot.latency_ms {
            Some(latency) if snapshot.connection.is_ready() => {
                format!("{}   ·   {latency} ms", snapshot.connection.label())
            }
            _ => snapshot.connection.label(),
        };
        ui.room_status.set_text(Some(&connection));
        let endpoint = {
            let settings = ui.settings.borrow();
            match snapshot.latency_ms {
                Some(latency) if snapshot.connection.is_ready() => {
                    format!("{}:{} · {latency} ms", settings.host, settings.port)
                }
                _ => format!("{}:{}", settings.host, settings.port),
            }
        };
        ui.server_line.set_text(Some(&endpoint));

        let levels = ui
            .voice
            .borrow()
            .as_ref()
            .map(|runtime| runtime.levels.borrow().clone())
            .unwrap_or_default();
        let stats = ui
            .voice
            .borrow()
            .as_ref()
            .map(|runtime| runtime.stats.borrow().clone())
            .unwrap_or_default();
        let mut member_rows: Vec<Ref<StackPanel>> = Vec::new();
        for member in &room.members {
            let is_self = member.session_id == room.self_member.session_id;
            let initial = member
                .name
                .chars()
                .next()
                .map(|ch| ch.to_uppercase().to_string())
                .unwrap_or_else(|| "?".to_owned());
            let avatar = Border::new();
            avatar.set_width(24.0);
            avatar.set_height(24.0);
            avatar.set_background(Some(ui_kit::brush(if is_self {
                0xFFD9_E7F5
            } else {
                0xFFE8_EEF5
            })));
            avatar.set_corner_radius(CornerRadius::uniform(12.0));
            let initial_text = ui_kit::body(&initial, 11.0);
            initial_text.set_horizontal_alignment(HorizontalAlignment::Center);
            initial_text.set_vertical_alignment(VerticalAlignment::Center);
            avatar.set_child(&initial_text);

            let mut name = member.name.clone();
            if is_self {
                name.push_str(" (you)");
            }
            let name_text = ui_kit::body(&name, 12.5);

            let states = ui_kit::row(4.0);
            if member.self_mute {
                states
                    .children()
                    .add(icons::icon(icons::Icon::MicrophoneOff, 13.0));
            }
            if member.sharing {
                states
                    .children()
                    .add(icons::icon(icons::Icon::ShareScreen, 13.0));
            }
            if member.voice {
                states
                    .children()
                    .add(icons::icon(icons::Icon::Microphone, 13.0));
            }
            if let Some(label) = stats.get(&member.session_id)
                && !label.is_empty()
            {
                states.children().add(ui_kit::muted(label, 10.5));
            }
            if speaking(&levels, if is_self { "" } else { &member.session_id }) {
                states.children().add(ui_kit::muted("\u{266A}", 12.0));
            }

            let row_grid = Grid::new();
            row_grid.set_column_definitions(ColumnDefinitions::from_items([
                ColumnDefinition::with_width(GridLength::AUTO),
                ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
                ColumnDefinition::with_width(GridLength::AUTO),
            ]));
            Grid::set_column(&avatar, 0);
            Grid::set_column(&name_text, 1);
            Grid::set_column(&states, 2);
            row_grid.children().add(&avatar);
            row_grid.children().add(&name_text);
            row_grid.children().add(&states);
            let row_panel = StackPanel::new();
            row_panel.children().add(&row_grid);
            member_rows.push(row_panel);
        }
        let members_signature = room
            .members
            .iter()
            .map(|member| {
                format!(
                    "{}:{}:{}:{}:{}",
                    member.session_id, member.name, member.self_mute, member.sharing, member.voice
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        if *ui.members_sig.borrow() != members_signature {
            *ui.members_sig.borrow_mut() = members_signature;
            ui.members_panel.children().clear();
            for row in &member_rows {
                ui.members_panel.children().add(row);
            }
        }

        let rooms_signature = snapshot
            .rooms
            .iter()
            .map(|room| format!("{}:{}:{}", room.id, room.member_count, room.name))
            .collect::<Vec<_>>()
            .join("|");
        if *ui.last_rooms.borrow() != rooms_signature {
            *ui.last_rooms.borrow_mut() = rooms_signature;
            ui.rooms_panel.children().clear();
            for summary in &snapshot.rooms {
                let active = summary.id == room.room.id;
                let hash = ui_kit::muted("#", 13.0);
                let name = ui_kit::body(&summary.name, 13.0);
                let count = ui_kit::muted(&summary.member_count.to_string(), 11.5);
                let row_grid = Grid::new();
                row_grid.set_column_definitions(ColumnDefinitions::from_items([
                    ColumnDefinition::with_width(GridLength::AUTO),
                    ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
                    ColumnDefinition::with_width(GridLength::AUTO),
                ]));
                Grid::set_column(&hash, 0);
                Grid::set_column(&name, 1);
                Grid::set_column(&count, 2);
                row_grid.children().add(&hash);
                row_grid.children().add(&name);
                row_grid.children().add(&count);

                let button = Button::new();
                button.set_content(Some(Rc::new(row_grid) as ferroui_base::BoxedValue));
                button.set_horizontal_content_alignment(HorizontalAlignment::Stretch);
                button.set_horizontal_alignment(HorizontalAlignment::Stretch);
                if active {
                    button.set_background(Some(ui_kit::brush(ui_kit::SOFT)));
                }
                let weak = Rc::downgrade(ui);
                let room_id = summary.id.clone();
                button.click(move |_, _| {
                    if let Some(ui) = weak.upgrade() {
                        let _ = ui.client.join(room_id.clone());
                    }
                });
                ui.rooms_panel.children().add(button);
            }
        }

        let last_id = snapshot
            .messages
            .last()
            .map(|message| message.message_id.as_str())
            .unwrap_or("");
        let appearance = {
            let settings = ui.settings.borrow();
            (settings.show_timestamps, settings.clock_24h)
        };
        let messages_signature = format!(
            "{}:{}:{}:{}:{}",
            room.room.id,
            snapshot.messages.len(),
            last_id,
            appearance.0,
            appearance.1
        );
        if *ui.last_messages.borrow() != messages_signature {
            *ui.last_messages.borrow_mut() = messages_signature;
            ui.feed_panel.children().clear();
            if snapshot.messages.is_empty() {
                ui.feed_panel
                    .children()
                    .add(ui_kit::muted("No messages yet.", 12.0));
            }
            let self_id = room.self_member.session_id.clone();
            let shown = snapshot.messages.len().saturating_sub(200);
            for message in snapshot.messages.iter().skip(shown) {
                let own = message.from == self_id;
                let header = if appearance.0 {
                    format!(
                        "{} · {}",
                        message.name,
                        local_clock(message.ts, appearance.1)
                    )
                } else {
                    message.name.clone()
                };
                let who = ui_kit::muted(&header, 11.0);
                let text = ui_kit::body(&message.text, 13.5);
                text.set_text_wrapping(ferroui_base::media::TextWrapping::Wrap);
                text.set_max_width(520.0);
                let bubble_inner = StackPanel::new();
                bubble_inner.set_spacing(3.0);
                bubble_inner.children().add(&who);
                bubble_inner.children().add(&text);
                let bubble = Border::new();
                bubble.set_background(Some(ui_kit::brush(if own {
                    0xFFEE_F4FB
                } else {
                    ui_kit::CARD
                })));
                bubble.set_border_brush(Some(ui_kit::brush(if own {
                    0xFFCF_E2F7
                } else {
                    ui_kit::STROKE
                })));
                bubble.set_border_thickness(Thickness::uniform(1.0));
                bubble.set_corner_radius(CornerRadius::uniform(10.0));
                bubble.set_padding(Thickness {
                    left: 12.0,
                    top: 7.0,
                    right: 12.0,
                    bottom: 7.0,
                });
                bubble.set_child(&bubble_inner);
                bubble.set_horizontal_alignment(if own {
                    HorizontalAlignment::Right
                } else {
                    HorizontalAlignment::Left
                });
                let line = StackPanel::new();
                line.set_horizontal_alignment(HorizontalAlignment::Stretch);
                line.children().add(&bubble);
                ui.feed_panel.children().add(&line);
            }
            ui.feed_scroll.scroll_to_end();
        }
    } else {
        ui.last_rooms.borrow_mut().clear();
        ui.last_messages.borrow_mut().clear();
    }
}

fn refresh_voice(ui: &Rc<Ui>, snapshot: &ChatSnapshot) {
    let active_room = snapshot.room.as_ref().map(|room| room.room.id.clone());
    let mut stop = None;
    if let Some(runtime) = ui.voice.borrow().as_ref() {
        if active_room.as_deref() != Some(runtime.room.as_str()) {
            stop = Some("left with the room");
        } else if !snapshot.connection.is_ready() {
            stop = Some("disconnected");
        }
    }
    if let Some(reason) = stop {
        let reconnect = reason == "disconnected";
        ui.leave_voice(Some(reason));
        if reconnect {
            // The WebSocket will come back; rejoin the same room's call automatically.
            ui.voice_wanted.set(true);
        } else {
            *ui.voice_room_id.borrow_mut() = None;
        }
    }
    let Some(room) = snapshot.room.as_ref() else {
        return;
    };

    let mut ended = None;
    let mut peer_error = None;
    {
        let mut slot = ui.voice.borrow_mut();
        if let Some(runtime) = slot.as_mut() {
            loop {
                match runtime.controller.try_recv() {
                    Some(VoiceEvent::Ready(_)) => {
                        *ui.voice_note.borrow_mut() = Some("live".to_owned());
                    }
                    Some(VoiceEvent::Level { peer, level }) => {
                        runtime
                            .levels
                            .borrow_mut()
                            .insert(peer, (level, Instant::now()));
                    }
                    Some(VoiceEvent::SelfLevel(level)) => {
                        runtime
                            .levels
                            .borrow_mut()
                            .insert(String::new(), (level, Instant::now()));
                    }
                    Some(VoiceEvent::Stats {
                        peer,
                        rtt_ms,
                        relayed,
                    }) => {
                        let label = match (relayed, rtt_ms) {
                            (true, Some(rtt)) => format!("relay {rtt} ms"),
                            (true, None) => "relay".to_owned(),
                            (false, Some(rtt)) => format!("{rtt} ms"),
                            (false, None) => String::new(),
                        };
                        runtime.stats.borrow_mut().insert(peer, label);
                    }
                    Some(VoiceEvent::ControlRequested { peer, lease_secs }) => {
                        *ui.control_request.borrow_mut() = Some((peer, lease_secs));
                        *ui.voice_note.borrow_mut() =
                            Some("a participant requests remote control".to_owned());
                    }
                    Some(VoiceEvent::ControlGranted { peer, lease_secs }) => {
                        ui.control_pending.set(false);
                        *ui.control_peer.borrow_mut() = Some(peer);
                        ui.control_until
                            .set(Some(Instant::now() + Duration::from_secs(lease_secs)));
                        *ui.voice_note.borrow_mut() = Some("remote control granted".to_owned());
                    }
                    Some(VoiceEvent::ControlHosting { peer, lease_secs }) => {
                        *ui.control_host_peer.borrow_mut() = Some(peer);
                        ui.control_host_until
                            .set(Some(Instant::now() + Duration::from_secs(lease_secs)));
                        *ui.voice_note.borrow_mut() = Some("remote control active".to_owned());
                    }
                    Some(VoiceEvent::ControlEnded { reason }) => {
                        ui.clear_control();
                        *ui.voice_note.borrow_mut() = Some(format!("control ended: {reason}"));
                    }
                    Some(VoiceEvent::FilePreparing { id, name }) => {
                        ui.file_names.borrow_mut().insert(id.clone(), name.clone());
                        *ui.file_active.borrow_mut() = Some(id);
                        *ui.file_line.borrow_mut() = format!("hashing {name}\u{2026}");
                    }
                    Some(VoiceEvent::FileOutgoing {
                        id,
                        peer,
                        name,
                        size,
                    }) => {
                        ui.file_names.borrow_mut().insert(id.clone(), name.clone());
                        *ui.file_active.borrow_mut() = Some(id);
                        *ui.file_line.borrow_mut() = format!(
                            "offering {name} ({}) to {}",
                            human_size(size),
                            ui.member_name(&peer)
                        );
                    }
                    Some(VoiceEvent::FileOffered {
                        id,
                        peer,
                        name,
                        size,
                        sha256: _,
                        caption,
                    }) => {
                        ui.file_names.borrow_mut().insert(id.clone(), name.clone());
                        ui.file_offers
                            .borrow_mut()
                            .push((id, peer.clone(), name.clone(), size));
                        let suffix = if caption.is_empty() {
                            String::new()
                        } else {
                            format!(" \u{2014} {caption}")
                        };
                        *ui.file_line.borrow_mut() = format!(
                            "{} offers {name} ({}){suffix}",
                            ui.member_name(&peer),
                            human_size(size)
                        );
                        ui.update_offer_panel();
                    }
                    Some(VoiceEvent::FileProgress {
                        id,
                        transferred,
                        total,
                    }) => {
                        *ui.file_active.borrow_mut() = Some(id.clone());
                        let name = ui
                            .file_names
                            .borrow()
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| "file".to_owned());
                        let percent = transferred
                            .saturating_mul(100)
                            .checked_div(total)
                            .unwrap_or(0);
                        *ui.file_line.borrow_mut() = format!(
                            "{name}: {percent}% \u{00b7} {}/{}",
                            human_size(transferred),
                            human_size(total)
                        );
                    }
                    Some(VoiceEvent::FileSent { id, path: _ }) => {
                        let name = ui
                            .file_names
                            .borrow()
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| "file".to_owned());
                        *ui.file_line.borrow_mut() = format!("sent {name}");
                        *ui.file_active.borrow_mut() = None;
                    }
                    Some(VoiceEvent::FileReceived { id, path }) => {
                        let name = ui
                            .file_names
                            .borrow()
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| "file".to_owned());
                        *ui.file_line.borrow_mut() = format!("saved {name} to {path}");
                        *ui.file_active.borrow_mut() = None;
                        *ui.file_last.borrow_mut() = Some(PathBuf::from(&path));
                        if ui.file_prefs.open_when_finished() {
                            open_path(&path);
                        }
                    }
                    Some(VoiceEvent::FileFailed { id, message }) => {
                        *ui.file_line.borrow_mut() = format!("file: {message}");
                        *ui.file_active.borrow_mut() = None;
                        if !id.is_empty() {
                            ui.file_offers.borrow_mut().retain(|offer| offer.0 != id);
                            ui.update_offer_panel();
                        }
                    }
                    Some(VoiceEvent::FileCancelled { id }) => {
                        *ui.file_line.borrow_mut() = "transfer cancelled".to_owned();
                        *ui.file_active.borrow_mut() = None;
                        ui.file_offers.borrow_mut().retain(|offer| offer.0 != id);
                        ui.update_offer_panel();
                    }
                    Some(VoiceEvent::PeerFailed(error)) => peer_error = Some(error),
                    Some(VoiceEvent::Error(error)) => {
                        ended = Some(format!("audio failed: {error}"));
                        break;
                    }
                    Some(VoiceEvent::Ended) => {
                        ended = Some("ended".to_owned());
                        break;
                    }
                    None => break,
                }
            }
            if ended.is_none() {
                let members: Vec<String> = room
                    .members
                    .iter()
                    .filter(|member| member.voice)
                    .map(|member| member.session_id.clone())
                    .collect();
                let signature = members.join(",");
                if *runtime.members_signature.borrow() != signature {
                    *runtime.members_signature.borrow_mut() = signature;
                    runtime.controller.send(VoiceCommand::Members(members));
                }
            }
        }
    }
    if let Some(reason) = ended {
        ui.leave_voice(Some(&reason));
    }
    if let Some(error) = peer_error {
        *ui.voice_note.borrow_mut() = Some(format!("a peer failed: {error}"));
    }

    let active = ui.voice.borrow().is_some();
    let voice_room = room.room.voice;
    if !active
        && voice_room
        && ui.voice_wanted.get()
        && snapshot.connection.is_ready()
        && ui.voice_room_id.borrow().as_deref() == Some(room.room.id.as_str())
    {
        ui.join_voice();
        return;
    }
    ui.join_voice.set_is_visible(voice_room && !active);
    ui.leave_voice.set_is_visible(active);
    ui.mute_mic.set_is_visible(active);
    ui.mute_speaker.set_is_visible(active);
    ui.share.set_is_visible(active);
    ui.share_source.set_is_visible(active);
    ui.share_quality.set_is_visible(active);
    ui.file_panel.set_is_visible(active);
    ui.file_card.set_is_visible(active);
    ui.share_card.set_is_visible(active);
    if active {
        ui.refresh_file_members(room);
    }
    ui.file_cancel
        .set_is_visible(ui.file_active.borrow().is_some());
    ui.file_open.set_is_visible(ui.file_last.borrow().is_some());
    let file_text = ui.file_line.borrow().clone();
    if *ui.file_shown.borrow() != file_text {
        *ui.file_shown.borrow_mut() = file_text.clone();
        ui.file_status.set_text(Some(&file_text));
    }

    let presenting = room
        .members
        .iter()
        .any(|member| member.sharing && member.session_id != room.self_member.session_id);
    let controlling = ui.controlling();
    let hosting = ui.control_host_peer.borrow().is_some();
    let pending = ui.control_pending.get();
    let requested = ui.control_request.borrow().is_some();
    ui.request_control
        .set_is_visible(active && presenting && !controlling && !hosting && !pending && !requested);
    ui.stop_control
        .set_is_visible(active && (controlling || hosting || pending));
    ui.allow_control.set_is_visible(requested && active);
    ui.deny_control.set_is_visible(requested && active);

    let control_text = if let Some((peer, _)) = ui.control_request.borrow().as_ref() {
        format!("{} requests remote control", ui.member_name(peer))
    } else if let Some(until) = ui.control_until.get() {
        let peer = ui.control_peer.borrow().clone().unwrap_or_default();
        format!(
            "controlling {} \u{2014} {}",
            ui.member_name(&peer),
            countdown(until.saturating_duration_since(Instant::now()).as_secs())
        )
    } else if let Some(until) = ui.control_host_until.get() {
        let peer = ui.control_host_peer.borrow().clone().unwrap_or_default();
        format!(
            "{} is controlling \u{2014} {}",
            ui.member_name(&peer),
            countdown(until.saturating_duration_since(Instant::now()).as_secs())
        )
    } else if pending {
        "waiting for approval".to_owned()
    } else {
        String::new()
    };
    if *ui.control_line.borrow() != control_text {
        *ui.control_line.borrow_mut() = control_text.clone();
        ui.control_status.set_text(Some(&control_text));
    }

    let note = ui
        .voice_note
        .borrow()
        .clone()
        .unwrap_or_else(|| "off \u{2014} join to talk".to_owned());
    let line = if voice_room || active {
        format!("voice: {note}")
    } else {
        String::new()
    };
    if *ui.voice_line.borrow() != line {
        *ui.voice_line.borrow_mut() = line.clone();
        ui.voice_status.set_text(Some(&line));
    }
}

fn render_video(ui: &Rc<Ui>, presenting: bool) {
    if !presenting {
        if ui.last_video.borrow_mut().take().is_some() {
            *ui.voice_note.borrow_mut() = Some("screen sharing stopped".to_owned());
        }
        ui.show_video(false);
        return;
    }
    let sink = ui
        .voice
        .borrow()
        .as_ref()
        .map(|runtime| runtime.controller.video());
    let mut fresh = false;
    if let Some(sink) = sink
        && let Some(frame) = sink.take()
    {
        let mut surface = ui.surface.borrow_mut();
        if surface
            .publish(frame.width, frame.height, &frame.bgra)
            .is_ok()
        {
            ui.presenter.present_latest();
            let first = ui.last_video.borrow().is_none();
            *ui.last_video.borrow_mut() = Some(Instant::now());
            if first {
                *ui.voice_note.borrow_mut() =
                    Some(format!("receiving screen {}x{}", frame.width, frame.height));
            }
            fresh = true;
        }
    }
    let recent = ui
        .last_video
        .borrow()
        .is_some_and(|at| at.elapsed() < Duration::from_millis(1500));
    ui.show_video(presenting && (fresh || recent));
}
fn speaking(levels: &HashMap<String, (f32, Instant)>, id: &str) -> bool {
    levels
        .get(id)
        .is_some_and(|(level, at)| at.elapsed() < Duration::from_millis(500) && *level > 0.012)
}
