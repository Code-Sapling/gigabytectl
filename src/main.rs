// src/main.rs

use std::{
    fs,
    io::{self, Stdout},
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
    process::Command
};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Clear, Dataset, GraphType, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
    symbols
};

const ROOT: &str = "/sys/devices/platform/aorus_laptop";
const FAN_MODE: &str = "/sys/devices/platform/aorus_laptop/fan_mode";
const FAN_CUSTOM_SPEED: &str = "/sys/devices/platform/aorus_laptop/fan_custom_speed";
const CHARGE_MODE: &str = "/sys/devices/platform/aorus_laptop/charge_mode";
const CHARGE_LIMIT: &str = "/sys/devices/platform/aorus_laptop/charge_limit";
const GPU_BOOST: &str = "/sys/devices/platform/aorus_laptop/gpu_boost";
const BATTERY_CYCLE: &str = "/sys/devices/platform/aorus_laptop/battery_cycle";
const FAN_CURVE_INDEX: &str = "/sys/devices/platform/aorus_laptop/fan_curve_index";
const FAN_CURVE_DATA: &str = "/sys/devices/platform/aorus_laptop/fan_curve_data";

const FAN_MODES: [&str; 6] = ["Normal", "Silent", "Gaming", "Custom", "Auto", "Fixed"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    FanMode,
    FanCustomSpeed,
    ChargeMode,
    ChargeLimit,
    GpuBoost,
    FanCurveView,
    FanCurveEdit,  
    Refresh,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditTarget {
    FanCustomSpeed,
    ChargeLimit,
    FanCurveTemp(usize),
    FanCurveSpeed(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Normal,
    Editing,
    FanCurveList,
}

struct App {
    items: &'static [Item],
    selected: usize,
    focus: Focus,
    status: String,
    input: String,
    editing: Option<EditTarget>,

    fan_mode: Option<i32>,
    fan_custom_speed: Option<i32>,
    charge_mode: Option<i32>,
    charge_limit: Option<i32>,
    gpu_boost: Option<i32>,
    battery_cycle: Option<String>,
    
    fan_curve: Option<Vec<(i32, i32)>>,
    fan_curve_selected: usize,
    fan_curve_col: usize, // 0 = Temp, 1 = Speed

    last_refresh: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            items: &[
                Item::FanMode,
                Item::FanCustomSpeed,
                Item::ChargeMode,
                Item::ChargeLimit,
                Item::GpuBoost,
                Item::FanCurveView,
                Item::FanCurveEdit,
                Item::Refresh,
                Item::Quit,
            ],
            selected: 0,
            focus: Focus::Normal,
            status: format!("Ready. Managing nodes in {}", ROOT),
            input: String::new(),
            editing: None,
            fan_mode: None,
            fan_custom_speed: None,
            charge_mode: None,
            charge_limit: None,
            gpu_boost: None,
            battery_cycle: None,
            fan_curve: None,
            fan_curve_selected: 0,
            fan_curve_col: 0,
            last_refresh: Instant::now(),
        }
    }

    fn refresh(&mut self) {
        self.fan_mode = read_i32(FAN_MODE);
        self.fan_custom_speed = read_i32(FAN_CUSTOM_SPEED);
        self.charge_mode = read_i32(CHARGE_MODE);
        self.charge_limit = read_i32(CHARGE_LIMIT);
        self.gpu_boost = read_i32(GPU_BOOST);
        self.battery_cycle = read_trimmed(BATTERY_CYCLE);
        self.fan_curve = read_fan_curve();
        self.last_refresh = Instant::now();
    }

    fn selected_item(&self) -> Item {
        self.items[self.selected]
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.items.len() as isize;
        self.selected = ((self.selected as isize + delta).rem_euclid(len)) as usize;
    }

    fn set_status<S: Into<String>>(&mut self, msg: S) {
        self.status = msg.into();
    }

    fn start_edit(&mut self, target: EditTarget, seed: Option<i32>) {
        self.focus = Focus::Editing;
        self.editing = Some(target);
        self.input = seed.map(|v| v.to_string()).unwrap_or_default();
    }

    fn cancel_edit(&mut self) {
        if let Some(EditTarget::FanCurveTemp(_)) | Some(EditTarget::FanCurveSpeed(_)) = self.editing {
            self.focus = Focus::FanCurveList;
        } else {
            self.focus = Focus::Normal;
        }
        self.editing = None;
        self.input.clear();
    }

    fn push_input(&mut self, c: char) {
        if c.is_ascii_digit() {
            self.input.push(c);
        }
    }

    fn backspace_input(&mut self) {
        self.input.pop();
    }

    fn apply_edit(&mut self) {
        let Some(target) = self.editing else { return; };
        let value: i32 = match self.input.trim().parse() {
            Ok(v) => v,
            Err(_) => {
                self.set_status("Invalid number");
                return;
            }
        };

        let result: Result<()> = match target {
            EditTarget::FanCustomSpeed => {
                if (25..=100).contains(&value) && value % 5 == 0 {
                    write_value(FAN_CUSTOM_SPEED, value)
                } else {
                    Err(anyhow::anyhow!("Fan speed must be 25..100 and divisible by 5"))
                }
            }
            EditTarget::ChargeLimit => {
                if (60..=100).contains(&value) {
                    write_value(CHARGE_LIMIT, value)
                } else {
                    Err(anyhow::anyhow!("Charge limit must be 60..100"))
                }
            }
            EditTarget::FanCurveTemp(idx) => {
                if (0..=100).contains(&value) {
                    if let Some(curve) = &self.fan_curve {
                        write_fan_curve_point(idx, value, curve[idx].1)
                    } else { Err(anyhow::anyhow!("Curve not loaded")) }
                } else {
                    Err(anyhow::anyhow!("Temperature must be 0..100"))
                }
            }
            EditTarget::FanCurveSpeed(idx) => {
                if (0..=255).contains(&value) {
                    if let Some(curve) = &self.fan_curve {
                        write_fan_curve_point(idx, curve[idx].0, value)
                    } else { Err(anyhow::anyhow!("Curve not loaded")) }
                } else {
                    Err(anyhow::anyhow!("Speed must be 0..255"))
                }
            }
        };

        match result {
            Ok(()) => {
                self.set_status(format!("Applied {}", value));
                self.cancel_edit();
                self.refresh();
            }
            Err(e) => self.set_status(e.to_string()),
        }
    }

    fn cycle(&mut self, path: &str, cur: Option<i32>, max: i32, step: isize, label: &str) {
        let next = (((cur.unwrap_or(0) as isize) + step).rem_euclid(max as isize)) as i32;
        match write_value(path, next) {
            Ok(()) => {
                self.set_status(format!("{} -> {}", label, next));
                self.refresh();
            }
            Err(e) => self.set_status(e.to_string()),
        }
    }

    fn toggle_gpu_boost(&mut self) {
        let next = match self.gpu_boost.unwrap_or(0) {
            1 => 0,
            _ => 1,
        };
        match write_value(GPU_BOOST, next) {
            Ok(()) => {
                self.set_status(format!("GPU boost -> {}", if next == 1 { "ON" } else { "OFF" }));
                self.refresh();
            }
            Err(e) => self.set_status(e.to_string()),
        }
    }
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn read_i32(path: &str) -> Option<i32> {
    read_trimmed(path).and_then(|s| s.parse::<i32>().ok())
}

fn write_value(path: &str, value: i32) -> Result<()> {
    if !Path::new(path).exists() {
        anyhow::bail!("Node not found: {}", path);
    }
    fs::write(path, format!("{}\n", value)).with_context(|| format!("write {} -> {}", value, path))?;
    Ok(())
}

fn read_fan_curve() -> Option<Vec<(i32, i32)>> {
    let mut curve = Vec::new();
    for i in 0..15 {
        if write_value(FAN_CURVE_INDEX, i).is_err() {
            return None;
        }
        let data = read_trimmed(FAN_CURVE_DATA)?;
        let parts: Vec<&str> = data.split_whitespace().collect();
        if parts.len() >= 2 {
            let temp = parts[0].parse::<i32>().ok()?;
            let speed = parts[1].parse::<i32>().ok()?;
            curve.push((temp, speed));
        } else {
            return None;
        }
    }
    Some(curve)
}

fn write_fan_curve_point(index: usize, temp: i32, speed: i32) -> Result<()> {
    write_value(FAN_CURVE_INDEX, index as i32)?;
    let data = (speed * 256) + temp;
    write_value(FAN_CURVE_DATA, data)?;
    Ok(())
}

fn fan_mode_name(v: Option<i32>) -> String {
    match v {
        Some(i) if (0..=5).contains(&i) => FAN_MODES[i as usize].to_string(),
        Some(i) => format!("Unknown ({})", i),
        None => "N/A".to_string(),
    }
}

fn gpu_boost_name(v: Option<i32>) -> &'static str {
    match v {
        Some(1) => "ON",
        Some(0) => "OFF",
        _ => "UNKNOWN",
    }
}

fn charge_mode_name(v: Option<i32>) -> &'static str {
    match v {
        Some(0) => "Normal",
        Some(1) => "Custom",
        _ => "UNKNOWN",
    }
}

fn value_or_na(v: Option<i32>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "N/A".to_string())
}

fn battery_cycle_text(v: Option<String>) -> String {
    match v.as_deref() {
        Some("0") => "Device does not support this feature".to_string(),
        Some(s) => s.to_string(),
        None => "N/A".to_string(),
    }
}

fn is_root() -> bool {
    fs::metadata("/proc/self").map(|m| m.uid() == 0).unwrap_or(false)
}

fn run_sudo() {
    let exe = std::env::current_exe().expect("failed to get current exe");

    let status = Command::new("sudo")
        .arg(exe)
        .args(std::env::args().skip(1))
        .status()
        .expect("failed to execute sudo");

    std::process::exit(status.code().unwrap_or(1));
}

fn driver_present() -> bool {
    Path::new(ROOT).exists()
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn item_title(item: Item) -> &'static str {
    match item {
        Item::FanMode => "Fan mode",
        Item::FanCustomSpeed => "Fan custom speed",
        Item::ChargeMode => "Charging mode",
        Item::ChargeLimit => "Charging limit",
        Item::GpuBoost => "GPU boost",
        Item::FanCurveView => "Fan curve (View)",
        Item::FanCurveEdit => "Fan curve (Edit)",      
        Item::Refresh => "Refresh values",
        Item::Quit => "Quit",
    }
}

fn item_hint(item: Item) -> &'static str {
    match item {
        Item::FanMode => "Left/Right to cycle names",
        Item::FanCustomSpeed => "Enter 25..100, divisible by 5",
        Item::ChargeMode => "Left/Right toggles Normal/Custom",
        Item::ChargeLimit => "Enter 60..100",
        Item::GpuBoost => "Left/Right toggles ON/OFF",
        Item::FanCurveView => "Shows a visual graph of the current fan curve",
        Item::FanCurveEdit => "Press Enter to edit the fan curve table",      
        Item::Refresh => "Reload all sysfs nodes",
        Item::Quit => "Exit the app",
    }
}

fn item_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    }
}

fn badge_style(text: &str) -> Style {
    match text {
        "ON" | "Custom" | "Gaming" | "Fixed" => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        "OFF" | "Normal" => Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD),
        "Silent" | "Auto" => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
    }
}

fn ui(frame: &mut ratatui::Frame<'_>, app: &App) {
    let area = frame.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(2),
        ])
        .split(area);

    let top = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(" gigabytectl ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("Gigabyte control panel", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(" root ", Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::raw(format!("last refresh: {}s ago", app.last_refresh.elapsed().as_secs())),
        ]),
    ])
    .block(Block::default().borders(Borders::ALL).style(Style::default().fg(Color::Cyan)));
    frame.render_widget(top, outer[0]);

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(outer[1]);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(4)])
        .split(main[0]);

    let items: Vec<ListItem> = app
        .items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let is_selected = idx == app.selected;
            let marker = if is_selected { "▶" } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} {}", marker, item_title(*item)), item_style(is_selected)),
            ]))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Controls"))
            .highlight_style(Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD)),
        left[0],
        &mut list_state,
    );

    let status_text = if app.focus == Focus::Editing {
        let label = match app.editing {
            Some(EditTarget::FanCustomSpeed) => "Enter fan custom speed".to_string(),
            Some(EditTarget::ChargeLimit) => "Enter charge limit".to_string(),
            Some(EditTarget::FanCurveTemp(idx)) => format!("Enter temp for idx {}", idx),
            Some(EditTarget::FanCurveSpeed(idx)) => format!("Enter speed for idx {}", idx),
            None => "Editing".to_string(),
        };
        format!("{}: {}", label, app.input)
    } else {
        app.status.clone()
    };

    frame.render_widget(
        Paragraph::new(status_text)
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .wrap(Wrap { trim: true }),
        left[1],
    );

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(12), Constraint::Length(5)])
        .split(main[1]);

    let selected = app.selected_item();

    if selected == Item::FanCurveView {
        // --- 1. VIEW MODE (Read-only Chart) ---
        if let Some(curve) = &app.fan_curve {
            let data_points: Vec<(f64, f64)> = curve
                .iter()
                .map(|&(t, s)| (t as f64, s as f64))
                .collect();

            let datasets = vec![Dataset::default()
                .name("Curve")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Cyan))
                .data(&data_points)];

            let x_axis = Axis::default()
                .title("Temp (°C)")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, 100.0])
                .labels(vec![Span::raw("0"), Span::raw("50"), Span::raw("100")]);

            let y_axis = Axis::default()
                .title("Speed")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, 255.0])
                .labels(vec![Span::raw("0"), Span::raw("128"), Span::raw("255")]);

            let chart = Chart::new(datasets)
                .block(Block::default().borders(Borders::ALL).title("Fan Curve Graph"))
                .x_axis(x_axis)
                .y_axis(y_axis);

            frame.render_widget(chart, right[0]);
        } else {
            let fc_widget = Paragraph::new("Failed to read fan curve data.")
                .block(Block::default().borders(Borders::ALL).title("Fan Curve Graph"));
            frame.render_widget(fc_widget, right[0]);
        }
    } else if app.focus == Focus::FanCurveList || selected == Item::FanCurveEdit {
        // --- 2. EDIT MODE (Interactive Table) ---
        let mut lines = vec![];
        lines.push(Line::from(vec![
            Span::styled(format!("{:>3}  ", "Idx"), Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:>9}", "Temp (°C)"), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("   "),
            Span::styled(format!("{:>13}", "Speed (0-255)"), Style::default().add_modifier(Modifier::BOLD)),
        ]));

        if let Some(curve) = &app.fan_curve {
            for (i, &(temp, speed)) in curve.iter().enumerate() {
                let is_selected_row = app.focus == Focus::FanCurveList && app.fan_curve_selected == i;
                let t_style = if is_selected_row && app.fan_curve_col == 0 { item_style(true) } else { Style::default() };
                let s_style = if is_selected_row && app.fan_curve_col == 1 { item_style(true) } else { Style::default() };

                lines.push(Line::from(vec![
                    Span::raw(format!("{:>3}  ", i)),
                    Span::styled(format!("{:>9}", temp), t_style),
                    Span::raw("   "),
                    Span::styled(format!("{:>13}", speed), s_style),
                ]));
            }
        } else {
            lines.push(Line::from("Failed to read fan curve data."));
        }

        let fc_widget = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Fan Curve Editor"))
            .wrap(Wrap { trim: true });
        frame.render_widget(fc_widget, right[0]);
    } else {
        let fan_mode_text = fan_mode_name(app.fan_mode);
        let gpu_text = gpu_boost_name(app.gpu_boost);
        let charge_text = charge_mode_name(app.charge_mode);

        let dashboard = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Fan mode       ", Style::default().fg(Color::White)),
                Span::styled(fan_mode_text.clone(), badge_style(&fan_mode_text)),
            ]),
            Line::from(vec![
                Span::styled("Fan speed      ", Style::default().fg(Color::White)),
                Span::styled(value_or_na(app.fan_custom_speed), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("Charge mode    ", Style::default().fg(Color::White)),
                Span::styled(charge_text, badge_style(charge_text)),
            ]),
            Line::from(vec![
                Span::styled("Charge limit   ", Style::default().fg(Color::White)),
                Span::styled(value_or_na(app.charge_limit), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("GPU boost      ", Style::default().fg(Color::White)),
                Span::styled(gpu_text, badge_style(gpu_text)),
            ]),
            Line::from(vec![
                Span::styled("Battery cycle  ", Style::default().fg(Color::White)),
                Span::styled(battery_cycle_text(app.battery_cycle.clone()), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            ]),
        ])
        .block(Block::default().borders(Borders::ALL).title("Current values"))
        .wrap(Wrap { trim: true });
        frame.render_widget(dashboard, right[0]);
    }

    let help_text = match app.focus {
        Focus::FanCurveList => vec![
            Line::from(vec![
                Span::styled("Editing: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled("Fan Curve", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("Hint: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw("Temp: 0-100, Speed: 0-255. Maintain non-decreasing order."),
            ]),
            Line::from("↑/↓ row   ←/→ col   Enter edit   Esc back"),
        ],
        _ => vec![
            Line::from(vec![
                Span::styled("Selected: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(item_title(selected), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("Hint: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(item_hint(selected)),
            ]),
            Line::from("↑/↓ move   ←/→ action   Enter edit/apply   Esc cancel   r refresh   q quit"),
        ]
    };
    let help = Paragraph::new(help_text)
        .block(Block::default().borders(Borders::ALL).title("Help"))
        .wrap(Wrap { trim: true });
    frame.render_widget(help, right[1]);

    let footer = Paragraph::new(if app.focus == Focus::Editing {
        "Editing mode: type numbers only, then press Enter"
    } else {
        "Ready"
    })
    .block(Block::default().borders(Borders::ALL).style(Style::default().fg(Color::DarkGray)));
    frame.render_widget(footer, outer[2]);

    if app.focus == Focus::Editing {
        let popup = centered_rect(56, 24, area);
        frame.render_widget(Clear, popup);
        let border_style = Style::default().fg(Color::Magenta);
        let popup_text = vec![
            Line::from(vec![
                Span::styled("Input", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled("(Esc cancels)", Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Value: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(app.input.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(""),
            Line::from("Fan speed: 25..100 in steps of 5"),
            Line::from("Charge limit: 60..100"),
            Line::from("Curve Temp: 0..100 | Curve Speed: 0..255"),
        ];
        frame.render_widget(
            Paragraph::new(popup_text)
                .block(Block::default().borders(Borders::ALL).title("Edit value").border_style(border_style))
                .wrap(Wrap { trim: true }),
            popup,
        );
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.focus == Focus::Editing {
        match key.code {
            KeyCode::Esc => app.cancel_edit(),
            KeyCode::Enter => app.apply_edit(),
            KeyCode::Backspace => app.backspace_input(),
            KeyCode::Char(c) => app.push_input(c),
            _ => {}
        }
        return false;
    }

    if app.focus == Focus::FanCurveList {
        match key.code {
            KeyCode::Esc => app.focus = Focus::Normal,
            KeyCode::Up => {
                if app.fan_curve_selected > 0 { app.fan_curve_selected -= 1; }
            }
            KeyCode::Down => {
                if app.fan_curve_selected < 14 { app.fan_curve_selected += 1; }
            }
            KeyCode::Left => {
                app.fan_curve_col = 0;
            }
            KeyCode::Right => {
                app.fan_curve_col = 1;
            }
            KeyCode::Enter => {
                if let Some(curve) = &app.fan_curve {
                    let val = if app.fan_curve_col == 0 { curve[app.fan_curve_selected].0 } else { curve[app.fan_curve_selected].1 };
                    let target = if app.fan_curve_col == 0 { EditTarget::FanCurveTemp(app.fan_curve_selected) } else { EditTarget::FanCurveSpeed(app.fan_curve_selected) };
                    app.start_edit(target, Some(val));
                }
            }
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('r') => {
            app.refresh();
            app.set_status("Refreshed values");
        }
        KeyCode::Up => app.move_selection(-1),
        KeyCode::Down => app.move_selection(1),
        KeyCode::Left => match app.selected_item() {
            Item::FanMode => app.cycle(FAN_MODE, app.fan_mode, 6, -1, "Fan mode"),
            Item::ChargeMode => app.cycle(CHARGE_MODE, app.charge_mode, 2, -1, "Charge mode"),
            Item::GpuBoost => app.toggle_gpu_boost(),
            _ => {}
        },
        KeyCode::Right => match app.selected_item() {
            Item::FanMode => app.cycle(FAN_MODE, app.fan_mode, 6, 1, "Fan mode"),
            Item::ChargeMode => app.cycle(CHARGE_MODE, app.charge_mode, 2, 1, "Charge mode"),
            Item::GpuBoost => app.toggle_gpu_boost(),
            _ => {}
        },
        KeyCode::Enter => match app.selected_item() {
            Item::FanMode => app.cycle(FAN_MODE, app.fan_mode, 6, 1, "Fan mode"),
            Item::FanCustomSpeed => app.start_edit(EditTarget::FanCustomSpeed, app.fan_custom_speed),
            Item::ChargeMode => app.cycle(CHARGE_MODE, app.charge_mode, 2, 1, "Charge mode"),
            Item::ChargeLimit => app.start_edit(EditTarget::ChargeLimit, app.charge_limit),
            Item::GpuBoost => app.toggle_gpu_boost(),
            Item::FanCurveEdit => app.focus = Focus::FanCurveList, // <-- Updated
            Item::FanCurveView => {}, // <-- Added (does nothing on Enter, just views)
            Item::Refresh => {
                app.refresh();
                app.set_status("Refreshed values");
            }
            Item::Quit => return true,
        },
        KeyCode::Char('e') => match app.selected_item() {
            Item::FanCustomSpeed => app.start_edit(EditTarget::FanCustomSpeed, app.fan_custom_speed),
            Item::ChargeLimit => app.start_edit(EditTarget::ChargeLimit, app.charge_limit),
            Item::FanCurveEdit => app.focus = Focus::FanCurveList, // <-- Updated
            _ => {}
        },        
        _ => {}
    }

    false
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    terminal::enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alternate screen")?;
    Terminal::new(CrosstermBackend::new(stdout)).context("create terminal")
}

fn restore_terminal(mut terminal: Terminal<CrosstermBackend<Stdout>>) {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

fn main() -> Result<()> {
    if !is_root() {
        println!("This program requires root privileges.");
        print!("Do you want to run with sudo? [Y/n]: ");

        use std::io::{self, Write};
        io::stdout().flush().ok();

        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();

        let input = input.trim().to_lowercase();

        if input.is_empty() || input == "y" {
            run_sudo();
        } else {
            println!("Exiting.");
            std::process::exit(1);
        }
    }

    if !driver_present() {
        eprintln!("{} does not exist. Please install gigabyte-laptop-wmi and ensure it is running.", ROOT);
        std::process::exit(1);
    }

    let mut terminal = setup_terminal()?;
    let mut app = App::new();
    app.refresh();

    let tick_rate = Duration::from_millis(250);
    let mut last_tick = Instant::now();

    let run = (|| -> Result<()> {
        loop {
            terminal.draw(|f| ui(f, &app)).context("draw ui")?;

            let timeout = tick_rate.saturating_sub(last_tick.elapsed());
            if event::poll(timeout).context("poll events")? {
                if let Event::Key(key) = event::read().context("read event")? {
                    if handle_key(&mut app, key) {
                        break;
                    }
                }
            }

            if last_tick.elapsed() >= tick_rate {
                last_tick = Instant::now();
            }
        }

        Ok(())
    })();

    restore_terminal(terminal);
    run
}