//! Window construction: the connect, room and settings views and their wiring.

use super::refresh::{refresh, smoke_request};
use super::settings::build_settings_screen;
use super::wire::{attach_control_input, wire};
use super::*;
use crate::platform::{system_scale, work_area};
use crate::{STARTUP, TIMER};

pub(crate) fn create_main_window() -> Ref<Window> {
    let Some(startup) = STARTUP.with(|cell| cell.borrow_mut().take()) else {
        panic!("the client was not initialized");
    };
    let settings = startup.settings.clone();
    let password = startup.password.clone();
    let settings_screen = build_settings_screen(&settings);

    let host = TextBox::new();
    host.set_text(Some(&settings.host));
    host.set_width(280.0);
    let port = TextBox::new();
    port.set_text(Some(&settings.port.to_string()));
    port.set_width(100.0);
    let name = TextBox::new();
    name.set_text(Some(&settings.name));
    name.set_width(220.0);
    let password_box = TextBox::new();
    password_box.set_password_char('•');
    password_box.set_width(260.0);
    if let Some(password) = &password {
        password_box.set_text(Some(password));
    }

    let tls = CheckBox::new();
    tls.set_content(Some(Rc::new("TLS".to_string())));
    tls.set_is_checked(Some(settings.tls));
    let remember = CheckBox::new();
    remember.set_content(Some(Rc::new("Remember password".to_string())));
    remember.set_is_checked(Some(settings.remember_password));

    let connect_button = Button::new();
    connect_button.set_content(Some(Rc::new("Connect".to_string())));
    let forget = icons::icon_button(icons::Icon::Shield, "Forget pinned key");
    let connect_status = text("not connected", 14.0);
    let error_text = text("", 13.0);

    let update_status = text("", 12.0);
    let update_check = icons::icon_button(icons::Icon::Restart, "Check for updates");
    let update_open = icons::icon_button(icons::Icon::OpenExternal, "Open release");
    update_open.set_is_visible(false);
    let update_auto = CheckBox::new();
    update_auto.set_content(Some(Rc::new("Check at startup".to_string())));
    update_auto.set_is_checked(Some(settings.check_for_updates));
    let update_row = row();
    update_row
        .children()
        .add(text(&format!("Rust Chat {APP_VERSION}"), 12.0));
    update_row.children().add(update_check.clone());
    update_row.children().add(update_open.clone());
    update_row.children().add(update_auto.clone());
    update_row.children().add(update_status.clone());
    settings_screen.about_slot.children().add(&update_row);
    let (update_tx, update_rx) = std::sync::mpsc::channel();
    if settings.check_for_updates {
        let tx = update_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(update::check(APP_VERSION));
        });
    }

    let trust_text = text("", 13.0);
    let approve = Button::new();
    approve.set_content(Some(Rc::new("Approve".to_string())));
    let deny = Button::new();
    deny.set_content(Some(Rc::new("Deny".to_string())));
    let trust_actions = row();
    trust_actions.children().add(approve.clone());
    trust_actions.children().add(deny.clone());
    let trust_panel = StackPanel::new();
    trust_panel.set_spacing(6.0);
    trust_panel.children().add(trust_text.clone());
    trust_panel.children().add(trust_actions);
    trust_panel.set_is_visible(false);

    // --- connect: brand panel + centred card ---
    host.set_width(250.0);
    port.set_width(80.0);
    name.set_width(340.0);
    password_box.set_width(340.0);
    for field in [&host, &port, &name, &password_box] {
        field.set_horizontal_alignment(HorizontalAlignment::Left);
    }
    tls.set_content(Some(Rc::new(String::new())));
    remember.set_content(Some(Rc::new(String::new())));
    let connect_label = ui_kit::body("Connect", 13.5);
    connect_label.set_foreground(Some(ui_kit::brush(0xFFFF_FFFF)));
    connect_button.set_content(Some(Rc::new(connect_label) as ferroui_base::BoxedValue));
    connect_button.set_background(Some(ui_kit::brush(ui_kit::ACCENT)));
    connect_button.set_horizontal_alignment(HorizontalAlignment::Stretch);

    let brand_mark = Border::new();
    brand_mark.set_width(46.0);
    brand_mark.set_height(46.0);
    brand_mark.set_background(Some(ui_kit::brush(0x33FF_FFFF)));
    brand_mark.set_corner_radius(CornerRadius::uniform(12.0));
    let brand_glyph = icons::icon(icons::Icon::Chat, 22.0);
    brand_glyph.set_foreground(Some(ui_kit::brush(0xFFFF_FFFF)));
    brand_mark.set_child(&brand_glyph);

    let brand_title = text("Rust Chat", 28.0);
    brand_title.set_foreground(Some(ui_kit::brush(0xFFFF_FFFF)));
    let brand_tag = ui_kit::muted(
        "Self-hosted chat with voice, screen sharing and peer-to-peer files. Your server, your data.",
        13.0,
    );
    brand_tag.set_foreground(Some(ui_kit::brush(0xFFD6_E6F7)));
    brand_tag.set_text_wrapping(ferroui_base::media::TextWrapping::Wrap);
    brand_tag.set_max_width(320.0);

    let brand = StackPanel::new();
    brand.set_spacing(10.0);
    brand.children().add(&brand_mark);
    brand.children().add(&brand_title);
    brand.children().add(&brand_tag);
    brand.children().add(ui_kit::muted("", 6.0));
    for (glyph, label) in [
        (icons::Icon::Chat, "Text, presence and local history"),
        (
            icons::Icon::Microphone,
            "Opus voice mesh with echo cancellation",
        ),
        (
            icons::Icon::ShareScreen,
            "Screen share, remote control, direct files",
        ),
        (
            icons::Icon::Shield,
            "TOFU pinning: approve the server key once",
        ),
    ] {
        let row = ui_kit::row(9.0);
        let icon = icons::icon(glyph, 15.0);
        icon.set_foreground(Some(ui_kit::brush(0xFFD6_E6F7)));
        row.children().add(&icon);
        let label = ui_kit::muted(label, 12.5);
        label.set_foreground(Some(ui_kit::brush(0xFFE6_F0FA)));
        row.children().add(&label);
        brand.children().add(&row);
    }
    let brand_frame = Border::new();
    brand_frame.set_background(Some(ui_kit::brush(0xFF0B_2F4D)));
    brand_frame.set_padding(Thickness::uniform(34.0));
    brand_frame.set_child(&brand);

    let card_title = ui_kit::headline("Connect to a server");
    let card_sub = ui_kit::muted(
        "Settings stay in %USERPROFILE%\\.rust-chat on this machine.",
        12.0,
    );
    let host_row = ui_kit::row(10.0);
    let host_field = StackPanel::new();
    host_field.set_spacing(4.0);
    host_field.children().add(ui_kit::muted("Server", 11.5));
    host_field.children().add(&host);
    let port_field = StackPanel::new();
    port_field.set_spacing(4.0);
    port_field.children().add(ui_kit::muted("Port", 11.5));
    port_field.children().add(&port);
    host_row.children().add(&host_field);
    host_row.children().add(&port_field);
    let name_field = StackPanel::new();
    name_field.set_spacing(4.0);
    name_field
        .children()
        .add(ui_kit::muted("Display name", 11.5));
    name_field.children().add(&name);
    let password_field = StackPanel::new();
    password_field.set_spacing(4.0);
    password_field
        .children()
        .add(ui_kit::muted("Password", 11.5));
    password_field.children().add(&password_box);
    let tls_row = ui_kit::row(8.0);
    tls_row.children().add(&tls);
    tls_row.children().add(ui_kit::body("Use TLS", 12.5));
    let remember_row = ui_kit::row(8.0);
    remember_row.children().add(&remember);
    remember_row
        .children()
        .add(ui_kit::body("Remember password", 12.5));
    let options = ui_kit::row(18.0);
    options.children().add(&tls_row);
    options.children().add(&remember_row);
    let actions = ui_kit::row(6.0);
    actions.children().add(&connect_button);
    actions.children().add(&settings_screen.open_connect);
    actions.children().add(&forget);

    let card = StackPanel::new();
    card.set_spacing(12.0);
    card.children().add(&card_title);
    card.children().add(&card_sub);
    card.children().add(&host_row);
    card.children().add(&name_field);
    card.children().add(&password_field);
    card.children().add(&options);
    card.children().add(&actions);
    card.children().add(&connect_status);
    card.children().add(&error_text);
    card.children().add(&trust_panel);
    let card_frame = ui_kit::card(&card, 26.0);
    card_frame.set_width(430.0);
    card_frame.set_horizontal_alignment(HorizontalAlignment::Center);
    card_frame.set_vertical_alignment(VerticalAlignment::Center);

    let connect_view = Grid::new();
    connect_view.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(400.0, GridUnitType::Pixel)),
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
    ]));
    let card_cell = Grid::new();
    card_cell.set_background(Some(ui_kit::brush(ui_kit::BG)));
    card_cell.children().add(&card_frame);
    Grid::set_column(&brand_frame, 0);
    Grid::set_column(&card_cell, 1);
    connect_view.children().add(&brand_frame);
    connect_view.children().add(&card_cell);

    let room_title = text("", 20.0);
    let room_status = text("", 13.0);
    let members_panel = StackPanel::new();
    members_panel.set_spacing(2.0);
    let rooms_panel = StackPanel::new();
    rooms_panel.set_spacing(4.0);
    let join_voice = icons::icon_button(icons::Icon::Phone, "Join voice");
    let leave_voice = icons::icon_button(icons::Icon::PhoneOff, "Leave voice");
    let mute_mic = icons::icon_button(icons::Icon::MicrophoneOff, "Mute microphone");
    let mute_speaker = icons::icon_button(icons::Icon::Speaker, "Mute speakers");
    let share = icons::icon_button(icons::Icon::ShareScreen, "Share screen");
    let share_sources = enumerate_sources();
    let share_source_index = settings
        .share_source
        .as_deref()
        .and_then(|id| share_sources.iter().position(|info| info.source.id() == id))
        .unwrap_or(0);
    let share_preset_index = SHARE_PRESETS
        .iter()
        .position(|preset| {
            preset.0 == settings.share_width
                && preset.1 == settings.share_height
                && preset.2 == settings.share_fps
        })
        .unwrap_or(1);
    let share_source = Button::new();
    share_source.set_content(Some(Rc::new(format!(
        "Source: {}",
        share_sources
            .get(share_source_index)
            .map(|info| info.label.as_str())
            .unwrap_or("Monitor 1")
    ))));
    let share_quality = Button::new();
    share_quality.set_content(Some(Rc::new(format!(
        "Quality: {}",
        preset_label(SHARE_PRESETS[share_preset_index])
    ))));
    let share_row = row();
    share_row.children().add(share_source.clone());
    share_row.children().add(share_quality.clone());
    let request_control = icons::icon_button(icons::Icon::RemoteControl, "Request remote control");
    request_control.set_is_visible(false);
    let stop_control = icons::icon_button(icons::Icon::Close, "Stop remote control");
    stop_control.set_is_visible(false);
    let allow_control = icons::icon_button(icons::Icon::Check, "Allow remote control");
    allow_control.set_is_visible(false);
    let deny_control = icons::icon_button(icons::Icon::Close, "Deny remote control");
    deny_control.set_is_visible(false);
    let control_status = text("", 13.0);
    let control_row = row();
    control_row.children().add(request_control.clone());
    control_row.children().add(stop_control.clone());
    control_row.children().add(allow_control.clone());
    control_row.children().add(deny_control.clone());
    control_row.children().add(control_status.clone());
    let voice_status = text("voice: off", 13.0);

    let surface = Rc::new(RefCell::new(
        VideoSurface::new().expect("the Direct2D video surface must be available"),
    ));
    let (presenter, presenter_sub) = build_presenter(surface.borrow().mailbox());
    presenter.set_width(640.0);
    presenter.set_height(360.0);
    presenter.set_horizontal_alignment(HorizontalAlignment::Center);
    presenter.set_is_visible(false);
    let video_host = Grid::new();
    video_host.set_width(640.0);
    video_host.set_height(360.0);
    video_host.set_horizontal_alignment(HorizontalAlignment::Center);
    video_host.set_background(Some(Brushes::transparent()));
    video_host.set_is_visible(false);
    video_host.children().add(presenter.clone());
    let video_label = text("SCREEN SHARE", 12.0);
    video_label.set_is_visible(false);

    let feed_panel = StackPanel::new();
    feed_panel.set_spacing(10.0);
    let feed_scroll = ScrollViewer::new();
    feed_scroll.set_vertical_scroll_bar_visibility(ScrollBarVisibility::Auto);
    feed_scroll.set_horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled);
    feed_scroll.set_content(Some(Control::boxed(feed_panel.clone())));
    let composer = TextBox::new();
    composer.set_accepts_return(false);
    composer.set_horizontal_alignment(HorizontalAlignment::Stretch);
    let send = icons::icon_button(icons::Icon::Send, "Send message");
    let composer_row = Grid::new();
    composer_row.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
        ColumnDefinition::with_width(GridLength::AUTO),
    ]));
    Grid::set_column(&composer, 0);
    Grid::set_column(&send, 1);
    composer_row.children().add(&composer);
    composer_row.children().add(&send);

    let file_members = StackPanel::new();
    file_members.set_orientation(Orientation::Horizontal);
    file_members.set_spacing(6.0);
    let file_path = TextBox::new();
    file_path.set_width(420.0);
    let file_caption = TextBox::new();
    file_caption.set_width(160.0);
    let file_choose = icons::icon_button(icons::Icon::Attach, "Choose a file");
    let file_send = icons::icon_button(icons::Icon::Upload, "Send file");
    let file_row = row();
    file_row.children().add(file_choose.clone());
    file_row.children().add(file_path.clone());
    file_row.children().add(file_caption.clone());
    file_row.children().add(file_send.clone());
    let file_offer_text = text("", 13.0);
    let file_accept = icons::icon_button(icons::Icon::Check, "Accept file");
    let file_decline = icons::icon_button(icons::Icon::Close, "Decline file");
    let file_offer_actions = row();
    file_offer_actions.children().add(file_accept.clone());
    file_offer_actions.children().add(file_decline.clone());
    let file_offer_panel = StackPanel::new();
    file_offer_panel.set_spacing(4.0);
    file_offer_panel.children().add(file_offer_text.clone());
    file_offer_panel.children().add(file_offer_actions);
    file_offer_panel.set_is_visible(false);
    let file_status = text("", 13.0);
    let file_cancel = icons::icon_button(icons::Icon::Close, "Cancel transfer");
    file_cancel.set_is_visible(false);
    let file_open = icons::icon_button(icons::Icon::Download, "Open received file");
    file_open.set_is_visible(false);
    let file_actions = row();
    file_actions.children().add(file_cancel.clone());
    file_actions.children().add(file_open.clone());
    file_actions.children().add(file_status.clone());
    let file_panel = StackPanel::new();
    file_panel.set_spacing(6.0);
    file_panel.children().add(text("FILES", 12.0));
    file_panel.children().add(file_members.clone());
    file_panel.children().add(file_row);
    file_panel.children().add(file_offer_panel.clone());
    file_panel.children().add(file_actions);
    file_panel.set_is_visible(false);

    let disconnect = icons::icon_button(icons::Icon::Disconnect, "Disconnect");
    let open_settings_room = settings_screen.open_room.clone();

    // ---- sidebar: brand, rooms, members, navigation ----
    let room_mark = Border::new();
    room_mark.set_width(30.0);
    room_mark.set_height(30.0);
    room_mark.set_background(Some(ui_kit::brush(ui_kit::ACCENT)));
    room_mark.set_corner_radius(CornerRadius::uniform(8.0));
    let mark_glyph = icons::icon(icons::Icon::Chat, 16.0);
    mark_glyph.set_foreground(Some(ui_kit::brush(0xFFFF_FFFF)));
    room_mark.set_child(&mark_glyph);

    let brand_text = StackPanel::new();
    brand_text.set_spacing(1.0);
    brand_text.children().add(ui_kit::body("Rust Chat", 14.0));
    let server_line = ui_kit::muted("", 11.0);
    brand_text.children().add(&server_line);
    let brand_row = ui_kit::row(9.0);
    brand_row.children().add(&room_mark);
    brand_row.children().add(&brand_text);

    let sidebar = StackPanel::new();
    sidebar.set_spacing(6.0);
    sidebar.children().add(&brand_row);
    sidebar.children().add(ui_kit::section_label("ROOMS"));
    sidebar.children().add(&rooms_panel);
    sidebar
        .children()
        .add(ui_kit::section_label("IN THIS ROOM"));
    sidebar.children().add(&members_panel);

    let sidebar_frame = Border::new();
    sidebar_frame.set_background(Some(ui_kit::brush(ui_kit::RAIL)));
    sidebar_frame.set_padding(Thickness {
        left: 12.0,
        top: 12.0,
        right: 12.0,
        bottom: 12.0,
    });
    sidebar_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    sidebar_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 0.0,
    });
    sidebar_frame.set_child(&sidebar);

    // ---- centre: header, stage, feed, composer ----
    let header_text = StackPanel::new();
    header_text.set_spacing(2.0);
    header_text.children().add(&room_title);
    header_text.children().add(&room_status);
    let header_actions = ui_kit::row(6.0);
    header_actions.children().add(&mute_mic);
    header_actions.children().add(&mute_speaker);
    header_actions.children().add(&share);
    header_actions.children().add(&join_voice);
    header_actions.children().add(&leave_voice);
    header_actions.children().add(&open_settings_room);
    header_actions.children().add(&disconnect);
    let header_grid = Grid::new();
    header_grid.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
        ColumnDefinition::with_width(GridLength::AUTO),
    ]));
    Grid::set_column(&header_text, 0);
    Grid::set_column(&header_actions, 1);
    header_grid.children().add(&header_text);
    header_grid.children().add(&header_actions);
    let header_frame = Border::new();
    header_frame.set_background(Some(ui_kit::brush(ui_kit::CARD)));
    header_frame.set_padding(Thickness {
        left: 14.0,
        top: 9.0,
        right: 14.0,
        bottom: 9.0,
    });
    header_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    header_frame.set_border_thickness(Thickness {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 1.0,
    });
    header_frame.set_child(&header_grid);

    video_label.set_foreground(Some(ui_kit::brush(0xFFCF_D6E0)));
    let stage_inner = StackPanel::new();
    stage_inner.set_spacing(6.0);
    stage_inner.children().add(&video_label);
    stage_inner.children().add(&video_host);
    let stage_frame = Border::new();
    stage_frame.set_background(Some(ui_kit::brush(0xFF0E_1116)));
    stage_frame.set_corner_radius(CornerRadius::uniform(10.0));
    stage_frame.set_padding(Thickness::uniform(8.0));
    stage_frame.set_margin(Thickness {
        left: 14.0,
        top: 10.0,
        right: 14.0,
        bottom: 0.0,
    });
    stage_frame.set_child(&stage_inner);

    let feed_frame = Border::new();
    feed_frame.set_background(Some(ui_kit::brush(ui_kit::CARD)));
    feed_frame.set_border_brush(Some(ui_kit::brush(ui_kit::STROKE)));
    feed_frame.set_border_thickness(Thickness::uniform(1.0));
    feed_frame.set_corner_radius(CornerRadius::uniform(10.0));
    feed_frame.set_padding(Thickness::uniform(8.0));
    feed_frame.set_margin(Thickness {
        left: 14.0,
        top: 10.0,
        right: 14.0,
        bottom: 0.0,
    });
    feed_frame.set_child(&feed_scroll);

    let composer_frame = Border::new();
    composer_frame.set_padding(Thickness {
        left: 14.0,
        top: 8.0,
        right: 14.0,
        bottom: 12.0,
    });
    composer_frame.set_child(&composer_row);

    // ---- secondary toolbar: voice and sharing controls ----
    let toolbar = ui_kit::row(10.0);
    toolbar.children().add(&voice_status);
    toolbar.children().add(&share_row);
    toolbar.children().add(&control_row);
    let toolbar_frame = Border::new();
    toolbar_frame.set_padding(Thickness {
        left: 14.0,
        top: 6.0,
        right: 14.0,
        bottom: 6.0,
    });
    toolbar_frame.set_child(&toolbar);

    let file_card = ui_kit::card(&file_panel, 10.0);
    file_card.set_margin(Thickness {
        left: 14.0,
        top: 8.0,
        right: 14.0,
        bottom: 0.0,
    });

    let centre = Grid::new();
    centre.set_row_definitions(RowDefinitions::from_items([
        RowDefinition::with_height(GridLength::AUTO),
        RowDefinition::with_height(GridLength::AUTO),
        RowDefinition::with_height(GridLength::AUTO),
        RowDefinition::with_height(GridLength::new(1.0, GridUnitType::Star)),
        RowDefinition::with_height(GridLength::AUTO),
        RowDefinition::with_height(GridLength::AUTO),
    ]));
    Grid::set_row(&header_frame, 0);
    Grid::set_row(&toolbar_frame, 1);
    Grid::set_row(&stage_frame, 2);
    Grid::set_row(&feed_frame, 3);
    Grid::set_row(&file_card, 4);
    Grid::set_row(&composer_frame, 5);
    centre.children().add(&header_frame);
    centre.children().add(&toolbar_frame);
    centre.children().add(&stage_frame);
    centre.children().add(&feed_frame);
    centre.children().add(&file_card);
    centre.children().add(&composer_frame);

    let room_view = Grid::new();
    room_view.set_column_definitions(ColumnDefinitions::from_items([
        ColumnDefinition::with_width(GridLength::new(236.0, GridUnitType::Pixel)),
        ColumnDefinition::with_width(GridLength::new(1.0, GridUnitType::Star)),
    ]));
    Grid::set_column(&sidebar_frame, 0);
    Grid::set_column(&centre, 1);
    room_view.children().add(&sidebar_frame);
    room_view.children().add(&centre);
    room_view.set_is_visible(false);

    // Hover hints: a hit-test invisible layer above every view.
    let tip_label = ui_kit::body("", 11.5);
    tip_label.set_foreground(Some(ui_kit::brush(0xFFFF_FFFF)));
    let tip_bubble = Border::new();
    tip_bubble.set_background(Some(ui_kit::brush(0xE61B_1B1B)));
    tip_bubble.set_corner_radius(CornerRadius::uniform(6.0));
    tip_bubble.set_padding(Thickness {
        left: 8.0,
        top: 4.0,
        right: 8.0,
        bottom: 4.0,
    });
    tip_bubble.set_child(&tip_label);
    tip_bubble.set_is_visible(false);
    Canvas::set_left(&tip_bubble, 0.0);
    Canvas::set_top(&tip_bubble, 0.0);
    let tip_layer = Canvas::new();
    tip_layer.set_horizontal_alignment(HorizontalAlignment::Stretch);
    tip_layer.set_vertical_alignment(VerticalAlignment::Stretch);
    tip_layer.set_is_hit_test_visible(false);
    tip_layer.children().add(&tip_bubble);

    let root = Grid::new();
    root.set_horizontal_alignment(HorizontalAlignment::Stretch);
    root.set_vertical_alignment(VerticalAlignment::Stretch);
    root.set_background(Some(ui_kit::brush(ui_kit::BG)));
    root.children().add(connect_view.clone());
    root.children().add(room_view.clone());
    root.children().add(settings_screen.view.clone());
    root.children().add(&tip_layer);

    let file_prefs = FilePrefs::new(
        settings.downloads_dir.as_ref().map(PathBuf::from),
        settings.open_when_finished,
    );

    let ui = Rc::new(Ui {
        client: startup.client,
        memory: startup.memory,
        runtime: startup.runtime,
        settings: RefCell::new(settings.clone()),
        connect_view: connect_view.clone(),
        room_view: room_view.clone(),
        stage_frame: stage_frame.clone(),
        file_card: file_card.clone(),
        tip_layer,
        tip_bubble,
        tip_label,
        server_line: server_line.clone(),
        host,
        port,
        name,
        password: password_box,
        tls,
        remember,
        connect_status,
        error_text,
        trust_panel,
        trust_text,
        room_title,
        room_status,
        members_panel,
        members_sig: RefCell::new(String::new()),
        rooms_panel,
        feed_panel,
        feed_scroll,
        composer,
        last_rooms: RefCell::new(String::new()),
        last_messages: RefCell::new(String::new()),
        connect_button,
        approve,
        deny,
        send,
        forget,
        disconnect,
        join_voice,
        leave_voice,
        mute_mic,
        mute_speaker,
        share,
        share_source,
        share_quality,
        share_sources: RefCell::new(share_sources),
        share_source_index: Cell::new(share_source_index),
        share_preset_index: Cell::new(share_preset_index),
        request_control,
        stop_control,
        allow_control,
        deny_control,
        control_status,
        control_pending: Cell::new(false),
        control_peer: RefCell::new(None),
        control_until: Cell::new(None),
        control_host_peer: RefCell::new(None),
        control_host_until: Cell::new(None),
        control_request: RefCell::new(None),
        last_mouse: Cell::new(Instant::now()),
        input_tokens: RefCell::new(Vec::new()),
        control_line: RefCell::new(String::new()),
        file_members,
        file_path,
        file_caption,
        file_send,
        file_choose,
        file_offer_panel,
        file_offer_text,
        file_accept,
        file_decline,
        file_status,
        file_cancel,
        file_open,
        file_panel,
        file_target: RefCell::new(None),
        file_offers: RefCell::new(Vec::new()),
        file_names: RefCell::new(HashMap::new()),
        file_active: RefCell::new(None),
        file_last: RefCell::new(None),
        file_members_sig: RefCell::new(String::new()),
        file_line: RefCell::new(String::new()),
        file_shown: RefCell::new(String::new()),
        update_status,
        update_check,
        update_open,
        update_auto,
        update_rx: RefCell::new(Some(update_rx)),
        update_tx,
        update_url: RefCell::new(None),
        presenter,
        video_host,
        surface,
        _presenter_sub: RefCell::new(Some(presenter_sub)),
        last_video: RefCell::new(None),
        video_visible: Cell::new(false),
        video_label,
        voice_status,
        voice_note: RefCell::new(None),
        voice_line: RefCell::new(String::new()),
        voice_wanted: Cell::new(false),
        voice_room_id: RefCell::new(None),
        voice: RefCell::new(None),
        smoke_voice: Cell::new((
            0,
            std::env::var("RUST_CHAT_SMOKE_VOICE")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
        )),
        smoke_voice_deadline: Cell::new(None),
        settings_screen,
        file_prefs,
    });

    wire(&ui);
    attach_control_input(&ui);
    let timer =
        DispatcherTimer::with_callback(Duration::from_millis(100), DispatcherPriority::NORMAL, {
            let ui = ui.clone();
            move |_| refresh(&ui)
        });
    TIMER.with(|cell| *cell.borrow_mut() = Some(timer));
    refresh(&ui);

    if settings.auto_connect && password.as_deref().is_some_and(|value| !value.is_empty()) {
        ui.connect();
    }
    if let Some(request) = smoke_request(&settings) {
        ui.host.set_text(Some(&request.host));
        ui.port.set_text(Some(&request.port.to_string()));
        ui.name.set_text(Some(&request.name));
        ui.password.set_text(Some(&request.password));
        ui.tls.set_is_checked(Some(request.tls));
        let _ = ui.client.connect(request);
    }
    if std::env::var_os("RUST_CHAT_OPEN_SETTINGS").is_some() {
        ui.set_settings_visible(true);
    }

    let window = Window::new();
    window.set_title(Some(format!("Rust Chat {APP_VERSION}")));
    let area = work_area();
    let scale = system_scale();
    let height = area
        .map(|area| (f64::from(area.height) / scale - 120.0).clamp(600.0, 920.0))
        .unwrap_or(920.0);
    let width = area
        .map(|area| (f64::from(area.width) / scale - 60.0).clamp(760.0, 1100.0))
        .unwrap_or(1100.0);
    window.set_width(width);
    window.set_height(height);
    if let Some(area) = area {
        // Center in the work area so a short screen never pushes the window under the taskbar.
        let outer_width = ((width + 16.0) * scale).round() as i32;
        let outer_height = ((height + 40.0) * scale).round() as i32;
        let x = area.left + (area.width as i32 - outer_width).max(0) / 2;
        let y = area.top + (area.height as i32 - outer_height).max(0) / 2;
        window.set_position(ferroui_base::PixelPoint { x, y });
    }
    window.set_padding(Thickness::uniform(18.0));
    window.set_content(Some(Control::boxed(root)));
    window
        .closed({
            let client = ui.client.clone();
            let weak = Rc::downgrade(&ui);
            move || {
                if let Some(ui) = weak.upgrade() {
                    // Keep the endpoint and display name even if the user never connected.
                    ui.save_connection_form();
                    ui.leave_voice(Some("voice: window closed"));
                }
                let _ = client.disconnect();
            }
        })
        .detach();
    window
}
