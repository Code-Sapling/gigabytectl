//! Rendering for the interactive TUI. Everything here is a pure function of
//! [`App`]; nothing touches the hardware.

use std::borrow::Cow;

use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Chart, Clear, Dataset, GraphType, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{App, CurveColumn, EditTarget, Focus, Item},
    config::Units,
    history::{History, Series},
    sysfs,
};

mod theme {
    use ratatui::style::{Color, Modifier, Style};

    const BOLD: Modifier = Modifier::BOLD;

    pub const ACCENT: Color = Color::Cyan;
    pub const LABEL: Style = Style::new().fg(Color::White);
    pub const VALUE: Style = Style::new().fg(Color::White).add_modifier(BOLD);
    pub const NUMBER: Style = Style::new().fg(Color::Magenta).add_modifier(BOLD);
    pub const INFO: Style = Style::new().fg(Color::Cyan).add_modifier(BOLD);
    pub const HEADING: Style = Style::new().fg(Color::Yellow).add_modifier(BOLD);
    pub const CPU: Style = Style::new().fg(Color::LightRed).add_modifier(BOLD);
    pub const GPU: Style = Style::new().fg(Color::Green).add_modifier(BOLD);
    pub const SELECTED: Style = Style::new().fg(Color::Black).bg(Color::Yellow).add_modifier(BOLD);
    pub const MUTED: Style = Style::new().fg(Color::DarkGray);
    pub const AXIS: Style = Style::new().fg(Color::Gray);
    pub const POPUP: Style = Style::new().fg(Color::Magenta);
    pub const POPUP_TITLE: Style = Style::new().fg(Color::Magenta).add_modifier(BOLD);
}

/// Colour for a mode/state badge, keyed on what the value means.
fn badge_style(text: &str) -> Style {
    match text {
        "ON" | "Custom" | "Gaming" | "Fixed" => theme::GPU,
        "OFF" | "Normal" => theme::CPU,
        "Silent" | "Auto" => theme::INFO,
        _ => theme::NUMBER,
    }
}

fn block(title: impl Into<String>) -> Block<'static> {
    Block::bordered().title(title.into())
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Both bars are three rows: two for the border, one for their text.
    let [header_area, body_area, footer_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(12), Constraint::Length(3)]).areas(area);
    let [left, right] = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(body_area);
    let [controls_area, status_area] = Layout::vertical([Constraint::Min(10), Constraint::Length(4)]).areas(left);
    let [detail_area, help_area] = Layout::vertical([Constraint::Min(12), Constraint::Length(5)]).areas(right);

    frame.render_widget(header(app), header_area);
    controls(frame, controls_area, app);
    frame.render_widget(status(app), status_area);
    detail(frame, detail_area, app);
    frame.render_widget(help(app), help_area);
    frame.render_widget(footer(app), footer_area);

    if let (Focus::Editing, Some(target)) = (app.focus, app.editing) {
        edit_popup(frame, area, &app.input, target);
    }
}

fn header(app: &App) -> Paragraph<'static> {
    const TAG: Modifier = Modifier::BOLD;
    Paragraph::new(Line::from(vec![
        Span::styled(
            " gigabytectl ",
            Style::new().fg(Color::Black).bg(theme::ACCENT).add_modifier(TAG),
        ),
        Span::raw("  "),
        Span::styled("Gigabyte control panel", theme::INFO),
        Span::raw("   "),
        Span::styled(" root ", Style::new().fg(Color::Black).bg(Color::Green).add_modifier(TAG)),
        Span::raw("  "),
        Span::raw(format!("last refresh: {}s ago", app.last_refresh.elapsed().as_secs())),
    ]))
    .block(Block::bordered().style(Style::new().fg(theme::ACCENT)))
}

fn controls(frame: &mut Frame, area: Rect, app: &App) {
    let list = List::new(Item::ALL.map(|item| ListItem::new(item.title())))
        .block(block("Controls"))
        .style(theme::LABEL)
        .highlight_symbol("▶ ")
        .highlight_style(theme::SELECTED);
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn status(app: &App) -> Paragraph<'static> {
    let text = match (app.focus, app.editing) {
        (Focus::Editing, Some(target)) => format!("{}: {}", target.prompt(), app.input),
        _ => app.status.clone(),
    };
    Paragraph::new(text).block(block("Status")).wrap(Wrap { trim: true })
}

fn footer(app: &App) -> Paragraph<'static> {
    let text = if app.focus == Focus::Editing {
        "Editing mode: type numbers only, then press Enter"
    } else {
        "Ready"
    };
    Paragraph::new(text).block(Block::bordered().style(theme::MUTED))
}

fn help(app: &App) -> Paragraph<'static> {
    let (subject, value, hint, keys) = match app.focus {
        Focus::FanCurveList => (
            "Editing: ",
            "Fan Curve",
            "Temp: 0-100, Speed: 0-255. Maintain non-decreasing order.",
            "↑/↓ row   ←/→ col   Enter edit   Esc back",
        ),
        _ => {
            let item = app.selected_item();
            (
                "Selected: ",
                item.title(),
                item.hint(),
                "↑/↓ move   ←/→ action   Enter edit/apply   Esc cancel   r refresh   q quit",
            )
        }
    };

    Paragraph::new(vec![
        Line::from(vec![Span::styled(subject, theme::HEADING), Span::styled(value, theme::VALUE)]),
        Line::from(vec![Span::styled("Hint: ", theme::INFO), Span::raw(hint)]),
        Line::from(keys),
    ])
    .block(block("Help"))
    .wrap(Wrap { trim: true })
}

// --- Detail panel ---

fn detail(frame: &mut Frame, area: Rect, app: &App) {
    match app.selected_item() {
        Item::FanCurveView => fan_curve_chart(frame, area, app),
        Item::History => history_charts(frame, area, &app.history, app.config.units),
        Item::FanCurveEdit => frame.render_widget(fan_curve_table(app), area),
        _ if app.focus == Focus::FanCurveList => frame.render_widget(fan_curve_table(app), area),
        _ => frame.render_widget(dashboard(app), area),
    }
}

/// One `label   value` line of the dashboard.
fn row<'a>(label: &str, value: impl Into<Cow<'a, str>>, style: Style) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<15}"), theme::LABEL),
        Span::styled(value, style),
    ])
}

fn dashboard(app: &App) -> Paragraph<'_> {
    let hw = &app.hw;
    let fan_mode = sysfs::fan_mode_name(hw.fan_mode);
    let charge_mode = sysfs::charge_mode_name(hw.charge_mode);
    let gpu_boost = sysfs::gpu_boost_name(hw.gpu_boost);
    let units = app.config.units;

    let mut lines = vec![
        row("Fan mode", fan_mode.clone(), badge_style(&fan_mode)),
        row("Fan speed", sysfs::value_or_na(hw.fan_custom_speed), theme::NUMBER),
        row("Charge mode", charge_mode.clone(), badge_style(&charge_mode)),
        row("Charge limit", sysfs::value_or_na(hw.charge_limit), theme::NUMBER),
        row("GPU boost", gpu_boost, badge_style(gpu_boost)),
        row("Battery cycle", hw.battery_cycle_text(), theme::INFO),
        row("Light sensor", hw.light_sensor_text(), Style::new().fg(theme::ACCENT)),
        row("Fan PWM", sysfs::value_or_na(hw.fan_pwm), theme::NUMBER),
        row("CPU temp", units.format(app.temps.cpu), theme::CPU),
        row("GPU temp", units.format(app.temps.gpu), theme::GPU),
    ];

    if !app.fans.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Fan readings:", theme::HEADING)));
        lines.extend(
            app.fans
                .iter()
                .map(|fan| row(&fan.name, format!("{} RPM", fan.rpm), theme::GPU)),
        );
    }

    Paragraph::new(lines)
        .block(block("Current values"))
        .wrap(Wrap { trim: true })
}

fn fan_curve_table(app: &App) -> Paragraph<'static> {
    let header = Line::from(vec![
        Span::styled(format!("{:>3}  ", "Idx"), theme::VALUE),
        Span::styled(format!("{:>9}", "Temp (°C)"), theme::VALUE),
        Span::raw("   "),
        Span::styled(format!("{:>13}", "Speed (0-255)"), theme::VALUE),
    ]);

    let mut lines = vec![header];
    match &app.fan_curve {
        Some(curve) => lines.extend(curve.iter().enumerate().map(|(index, &(temp, speed))| {
            let active = app.focus == Focus::FanCurveList && app.curve_row == index;
            let cell = |column: CurveColumn| {
                if active && app.curve_column == column {
                    theme::SELECTED
                } else {
                    Style::new()
                }
            };
            Line::from(vec![
                Span::raw(format!("{index:>3}  ")),
                Span::styled(format!("{temp:>9}"), cell(CurveColumn::Temp)),
                Span::raw("   "),
                Span::styled(format!("{speed:>13}"), cell(CurveColumn::Speed)),
            ])
        })),
        None => lines.push(Line::from("Failed to read fan curve data.")),
    }

    // Deliberately not wrapped: wrapping trims the leading padding, which
    // knocks the two-digit rows out of their columns.
    Paragraph::new(lines).block(block("Fan Curve Editor"))
}

fn fan_curve_chart(frame: &mut Frame, area: Rect, app: &App) {
    let Some(curve) = &app.fan_curve else {
        frame.render_widget(placeholder("Failed to read fan curve data.", "Fan Curve Graph"), area);
        return;
    };

    let points: Vec<(f64, f64)> = curve.iter().map(|&(t, s)| (t.into(), s.into())).collect();
    let chart = Chart::new(vec![dataset("Curve", theme::ACCENT, &points)])
        .block(block("Fan Curve Graph"))
        .x_axis(axis([0.0, 100.0], ticks([0.0, 100.0], "")).title("Temp (°C)"))
        .y_axis(axis([0.0, 255.0], ticks([0.0, 255.0], "")).title("Speed"));
    frame.render_widget(chart, area);
}

fn history_charts(frame: &mut Frame, area: Rect, history: &History, units: Units) {
    let [top, bottom] = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);

    let convert = |series: &Series| -> Vec<(f64, f64)> {
        series
            .points()
            .into_iter()
            .map(|(t, v)| (t, units.convert(v)))
            .collect()
    };
    let cpu = convert(&history.cpu);
    let gpu = convert(&history.gpu);

    let temp_title = format!("Temperature ({})", units.symbol().trim_start_matches('°'));
    match bounds(&[&cpu, &gpu]) {
        Some((x, y)) => {
            // Keep the line off the border even when the readings are flat.
            let pad = ((y[1] - y[0]) * 0.1).max(2.0);
            let y = [(y[0] - pad).max(0.0), y[1] + pad];
            let chart = Chart::new(vec![
                dataset("CPU", Color::LightRed, &cpu),
                dataset("GPU", Color::Green, &gpu),
            ])
            .block(block(&temp_title))
            .x_axis(axis(x, ticks(x, "s")))
            .y_axis(axis(y, ticks(y, units.symbol())));
            frame.render_widget(chart, top);
        }
        None => frame.render_widget(placeholder("Collecting temperature samples...", &temp_title), top),
    }

    let rpm = history.rpm.points();
    match bounds(&[&rpm]) {
        Some((x, y)) => {
            let y = [0.0, y[1] * 1.1 + 1.0];
            let chart = Chart::new(vec![dataset("Fan RPM", theme::ACCENT, &rpm)])
                .block(block("Fan RPM (max)"))
                .x_axis(axis(x, ticks(x, "s")))
                .y_axis(axis(y, ticks(y, "")));
            frame.render_widget(chart, bottom);
        }
        None => frame.render_widget(placeholder("Collecting fan samples...", "Fan RPM (max)"), bottom),
    }
}

fn edit_popup(frame: &mut Frame, area: Rect, input: &str, target: EditTarget) {
    let popup = centered_rect(56, 24, area);
    let range = target.range();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(target.prompt(), theme::POPUP_TITLE),
                Span::raw("  "),
                Span::styled("(Esc cancels)", theme::MUTED),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Value: ", theme::HEADING),
                Span::styled(input.to_string(), theme::VALUE),
            ]),
            Line::from(""),
            Line::from(format!("Allowed: {}..{}", range.start(), range.end())),
        ])
        .block(block("Edit value").border_style(theme::POPUP))
        .wrap(Wrap { trim: true }),
        popup,
    );
}

// --- Chart helpers ---

fn dataset<'a>(name: &'a str, color: Color, data: &'a [(f64, f64)]) -> Dataset<'a> {
    Dataset::default()
        .name(name)
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::new().fg(color))
        .data(data)
}

fn axis(range: [f64; 2], labels: Vec<Span<'_>>) -> Axis<'_> {
    Axis::default().style(theme::AXIS).bounds(range).labels(labels)
}

/// Low / middle / high tick labels for an axis.
fn ticks(range: [f64; 2], suffix: &str) -> Vec<Span<'static>> {
    let mid = f64::midpoint(range[0], range[1]);
    Vec::from([range[0], mid, range[1]].map(|value| Span::raw(format!("{value:.0}{suffix}"))))
}

/// The `[x, y]` extents spanning every point in `datasets`, or `None` when
/// there is nothing to plot yet.
fn bounds(datasets: &[&[(f64, f64)]]) -> Option<([f64; 2], [f64; 2])> {
    let mut x = [f64::MAX, f64::MIN];
    let mut y = [f64::MAX, f64::MIN];
    let mut any = false;
    for &(px, py) in datasets.iter().flat_map(|data| data.iter()) {
        any = true;
        x = [x[0].min(px), x[1].max(px)];
        y = [y[0].min(py), y[1].max(py)];
    }
    any.then_some((x, y))
}

fn placeholder<'a>(text: &'a str, title: &'a str) -> Paragraph<'a> {
    Paragraph::new(text).block(block(title))
}

/// A rectangle covering `percent_x` × `percent_y` of `area`, centred.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [area] = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .areas(area);
    area
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    use crate::{app::App, config::Config};

    #[test]
    fn bounds_span_every_dataset() {
        let a = [(0.0, 10.0), (5.0, 30.0)];
        let b = [(2.0, 5.0)];
        let (x, y) = bounds(&[&a, &b]).unwrap();
        assert_eq!(x, [0.0, 5.0]);
        assert_eq!(y, [5.0, 30.0]);
        assert_eq!(bounds(&[&[], &[]]), None);
    }

    #[test]
    fn ticks_are_low_mid_high() {
        let labels: Vec<String> = ticks([0.0, 100.0], "s").iter().map(|s| s.content.to_string()).collect();
        assert_eq!(labels, ["0s", "50s", "100s"]);
    }

    #[test]
    fn centered_rect_is_centered_and_smaller() {
        let area = Rect::new(0, 0, 100, 100);
        let popup = centered_rect(50, 20, area);
        assert_eq!((popup.width, popup.height), (50, 20));
        assert_eq!((popup.x, popup.y), (25, 40));
    }

    /// Rendering must not panic on tiny terminals or with no data yet.
    #[test]
    fn every_view_renders_at_several_sizes() {
        for (width, height) in [(120, 40), (80, 24), (40, 12), (20, 8)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut app = App::new(Config::default());
            app.fan_curve = Some(vec![(30, 40); sysfs::FAN_CURVE_POINTS]);
            for index in 0..Item::ALL.len() {
                app.selected = index;
                terminal.draw(|frame| draw(frame, &app)).unwrap();
            }
            app.focus = Focus::FanCurveList;
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            app.focus = Focus::Editing;
            for target in [
                EditTarget::FanCustomSpeed,
                EditTarget::ChargeLimit,
                EditTarget::FanCurve(2, CurveColumn::Speed),
            ] {
                app.editing = Some(target);
                app.input = "123".to_string();
                terminal.draw(|frame| draw(frame, &app)).unwrap();
            }
        }
    }
}
