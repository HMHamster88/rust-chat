//! Design tokens and the small building blocks the shell is made of.
//!
//! Colours follow a Fluent-like light palette: a `#f3f3f3` window, white cards with a 1 px
//! stroke, one accent, and muted secondary text.

#![allow(dead_code)]

use ferroui_base::media::IBrush;
use ferroui_base::{CornerRadius, Ref, Thickness};
use ferroui_controls::{Border, Control, StackPanel, TextBlock};
use std::rc::Rc;

pub const BG: u32 = 0xFFF3_F3F3;
pub const CARD: u32 = 0xFFFF_FFFF;
pub const STROKE: u32 = 0xFFE3_E3E3;
pub const TEXT: u32 = 0xFF1B_1B1B;
pub const MUTED: u32 = 0xFF61_6161;
pub const ACCENT: u32 = 0xFF0F_6CBD;
pub const SOFT: u32 = 0xFFEE_F4FB;
pub const RAIL: u32 = 0xFFFA_FAFA;
pub const DANGER: u32 = 0xFFC4_2B1C;
pub const FIELD: u32 = 0xFFFB_FBFB;

/// A solid brush from a 0xAARRGGBB value.
pub fn brush(argb: u32) -> Rc<dyn IBrush> {
    let color = ferroui_base::media::SolidColorBrush::from_uint32(argb);
    color.to_immutable() as Rc<dyn IBrush>
}

/// A white rounded card with a 1 px stroke and inner padding.
pub fn card(child: impl Into<ferroui_base::Nullable<Control>>, padding: f64) -> Ref<Border> {
    let border = Border::new();
    border.set_background(Some(brush(CARD)));
    border.set_border_brush(Some(brush(STROKE)));
    border.set_border_thickness(Thickness::uniform(1.0));
    border.set_corner_radius(CornerRadius::uniform(10.0));
    border.set_padding(Thickness::uniform(padding));
    border.set_child(child);
    border
}

/// A card whose content is a vertical stack.
pub fn card_column(padding: f64, spacing: f64) -> (Ref<Border>, Ref<StackPanel>) {
    let column = StackPanel::new();
    column.set_spacing(spacing);
    let border = card(&column, padding);
    (border, column)
}

/// A horizontal, vertically centred row.
pub fn row(spacing: f64) -> Ref<StackPanel> {
    let row = StackPanel::new();
    row.set_orientation(ferroui_base::layout::Orientation::Horizontal);
    row.set_spacing(spacing);
    row.set_vertical_alignment(ferroui_base::layout::VerticalAlignment::Center);
    row
}

/// A 1 px separator, stretched horizontally.
pub fn divider() -> Ref<Border> {
    let line = Border::new();
    line.set_height(1.0);
    line.set_background(Some(brush(STROKE)));
    line.set_horizontal_alignment(ferroui_base::layout::HorizontalAlignment::Stretch);
    line
}

/// Text with the primary colour.
pub fn body(label: &str, size: f64) -> Ref<TextBlock> {
    let block = super::text(label, size);
    block.set_foreground(Some(brush(TEXT)));
    block
}

/// Secondary text, used for hints and metadata.
pub fn muted(label: &str, size: f64) -> Ref<TextBlock> {
    let block = super::text(label, size);
    block.set_foreground(Some(brush(MUTED)));
    block
}

/// All-caps section label above a card.
pub fn section_label(label: &str) -> Ref<TextBlock> {
    let block = super::text(label, 11.0);
    block.set_foreground(Some(brush(MUTED)));
    block
}

/// A screen headline.
pub fn headline(label: &str) -> Ref<TextBlock> {
    let block = super::text(label, 20.0);
    block.set_foreground(Some(brush(TEXT)));
    block
}

/// A small accent-tinted pill, e.g. the connection state.
pub fn pill(label: &str) -> Ref<Border> {
    let text = super::text(label, 11.5);
    text.set_foreground(Some(brush(0xFF0F_7B0F)));
    let border = Border::new();
    border.set_background(Some(brush(0xFFEA_F6EA)));
    border.set_border_brush(Some(brush(0xFFCF_E9CF)));
    border.set_border_thickness(Thickness::uniform(1.0));
    border.set_corner_radius(CornerRadius::uniform(20.0));
    border.set_padding(Thickness {
        left: 9.0,
        top: 2.0,
        right: 9.0,
        bottom: 2.0,
    });
    border.set_child(&text);
    border.set_horizontal_alignment(ferroui_base::layout::HorizontalAlignment::Left);
    border
}
