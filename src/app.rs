//! State and input handling for the interactive TUI.

use std::{ops::RangeInclusive, time::Instant};

use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    config::Config,
    history::History,
    sensors::{Fan, Sensors, Temps},
    sysfs::{self, HwState},
};

/// Whether the event loop should keep running after a key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Exit,
}

/// A row in the control list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    FanMode,
    FanCustomSpeed,
    ChargeMode,
    ChargeLimit,
    GpuBoost,
    FanCurveView,
    FanCurveEdit,
    History,
    Refresh,
    Quit,
}

impl Item {
    pub const ALL: [Self; 10] = [
        Self::FanMode,
        Self::FanCustomSpeed,
        Self::ChargeMode,
        Self::ChargeLimit,
        Self::GpuBoost,
        Self::FanCurveView,
        Self::FanCurveEdit,
        Self::History,
        Self::Refresh,
        Self::Quit,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::FanMode => "Fan mode",
            Self::FanCustomSpeed => "Fan custom speed",
            Self::ChargeMode => "Charging mode",
            Self::ChargeLimit => "Charging limit",
            Self::GpuBoost => "GPU boost",
            Self::FanCurveView => "Fan curve (View)",
            Self::FanCurveEdit => "Fan curve (Edit)",
            Self::History => "History graph",
            Self::Refresh => "Refresh values",
            Self::Quit => "Quit",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::FanMode => "Left/Right to cycle names",
            Self::FanCustomSpeed => "Enter 0..255",
            Self::ChargeMode => "Left/Right toggles Normal/Custom",
            Self::ChargeLimit => "Enter 60..100",
            Self::GpuBoost => "Left/Right toggles ON/OFF",
            Self::FanCurveView => "Shows a visual graph of the current fan curve",
            Self::FanCurveEdit => "Press Enter to edit the fan curve table",
            Self::History => "Live CPU/GPU temperature and fan RPM over time",
            Self::Refresh => "Reload all sysfs nodes",
            Self::Quit => "Exit the app",
        }
    }

    /// True while this item shows the fan curve, which is only read from the
    /// device when something is displaying it.
    fn shows_fan_curve(self) -> bool {
        matches!(self, Self::FanCurveView | Self::FanCurveEdit)
    }
}

/// Which column of the fan curve table is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveColumn {
    Temp,
    Speed,
}

/// The value currently being typed into the edit popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditTarget {
    FanCustomSpeed,
    ChargeLimit,
    FanCurve(usize, CurveColumn),
}

impl EditTarget {
    /// The values the device accepts for this field.
    pub fn range(self) -> RangeInclusive<i32> {
        match self {
            Self::FanCustomSpeed => sysfs::FAN_SPEED_RANGE,
            Self::ChargeLimit => sysfs::CHARGE_LIMIT_RANGE,
            Self::FanCurve(_, CurveColumn::Temp) => sysfs::CURVE_TEMP_RANGE,
            Self::FanCurve(_, CurveColumn::Speed) => sysfs::CURVE_SPEED_RANGE,
        }
    }

    pub fn prompt(self) -> String {
        match self {
            Self::FanCustomSpeed => "Enter fan custom speed".to_string(),
            Self::ChargeLimit => "Enter charge limit".to_string(),
            Self::FanCurve(index, CurveColumn::Temp) => format!("Enter temp for idx {index}"),
            Self::FanCurve(index, CurveColumn::Speed) => format!("Enter speed for idx {index}"),
        }
    }
}

/// Which part of the UI has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Normal,
    Editing,
    FanCurveList,
}

pub struct App {
    pub selected: usize,
    pub focus: Focus,
    pub status: String,
    pub input: String,
    pub editing: Option<EditTarget>,

    pub hw: HwState,
    pub fan_curve: Option<Vec<(i32, i32)>>,
    pub curve_row: usize,
    pub curve_column: CurveColumn,

    pub fans: Vec<Fan>,
    pub temps: Temps,
    pub history: History,
    pub config: Config,
    pub last_refresh: Instant,

    sensors: Sensors,
}

impl App {
    pub fn new(config: Config) -> Self {
        let mut app = Self {
            selected: 0,
            focus: Focus::Normal,
            status: format!("Ready. Managing nodes in {}", sysfs::ROOT),
            input: String::new(),
            editing: None,
            hw: HwState::default(),
            fan_curve: None,
            curve_row: 0,
            curve_column: CurveColumn::Temp,
            fans: Vec::new(),
            temps: Temps::default(),
            history: History::new(config.history_length),
            config,
            last_refresh: Instant::now(),
            sensors: Sensors::new(),
        };
        app.refresh();
        app
    }

    pub fn selected_item(&self) -> Item {
        Item::ALL[self.selected]
    }

    /// Re-reads everything the UI is currently showing.
    pub fn refresh(&mut self) {
        self.hw = HwState::read();
        self.fans = self.sensors.read_fans();
        self.temps = self.sensors.read_temps();
        self.history.push(self.temps, &self.fans);
        if self.shows_fan_curve() {
            self.reload_fan_curve();
        }
        self.last_refresh = Instant::now();
    }

    fn shows_fan_curve(&self) -> bool {
        self.focus == Focus::FanCurveList || self.selected_item().shows_fan_curve()
    }

    /// Reading the curve means writing an index to the device fifteen times, so
    /// it only happens while the curve is on screen.
    fn reload_fan_curve(&mut self) {
        self.fan_curve = sysfs::read_fan_curve().ok();
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status = message.into();
    }

    fn move_selection(&mut self, delta: isize) {
        let count = Item::ALL.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(count) as usize;
        // Load the curve as soon as its view is selected, not on the next tick.
        if self.shows_fan_curve() && self.fan_curve.is_none() {
            self.reload_fan_curve();
        }
    }

    fn start_edit(&mut self, target: EditTarget, seed: Option<i32>) {
        self.focus = Focus::Editing;
        self.editing = Some(target);
        self.input = seed.map(|value| value.to_string()).unwrap_or_default();
    }

    fn cancel_edit(&mut self) {
        self.focus = match self.editing {
            Some(EditTarget::FanCurve(..)) => Focus::FanCurveList,
            _ => Focus::Normal,
        };
        self.editing = None;
        self.input.clear();
    }

    /// Writes the typed value to the device, keeping the popup open on error so
    /// the entry can be corrected.
    fn apply_edit(&mut self) {
        let Some(target) = self.editing else { return };
        let Ok(value) = self.input.trim().parse::<i32>() else {
            self.set_status("Invalid number");
            return;
        };

        let result = match target {
            EditTarget::FanCustomSpeed => {
                sysfs::validate_fan_speed(value).and_then(|()| sysfs::write_value(sysfs::FAN_CUSTOM_SPEED, value))
            }
            EditTarget::ChargeLimit => {
                sysfs::validate_charge_limit(value).and_then(|()| sysfs::write_value(sysfs::CHARGE_LIMIT, value))
            }
            EditTarget::FanCurve(index, column) => self.write_curve_value(index, column, value),
        };

        match result {
            Ok(()) => {
                self.set_status(format!("Applied {value}"));
                self.cancel_edit();
                self.refresh();
            }
            Err(e) => self.set_status(format!("{e:#}")),
        }
    }

    /// Rewrites one curve point, keeping the column that was not edited.
    fn write_curve_value(&self, index: usize, column: CurveColumn, value: i32) -> Result<()> {
        let &(temp, speed) = self
            .fan_curve
            .as_ref()
            .and_then(|curve| curve.get(index))
            .ok_or_else(|| anyhow!("Curve not loaded"))?;
        match column {
            CurveColumn::Temp => sysfs::write_fan_curve_point(index, value, speed),
            CurveColumn::Speed => sysfs::write_fan_curve_point(index, temp, value),
        }
    }

    /// Steps a named node forwards or backwards through `names`, wrapping around.
    fn cycle(&mut self, node: &str, current: Option<i32>, names: &[&'static str], delta: isize, label: &str) {
        let next = (current.unwrap_or(0) as isize + delta).rem_euclid(names.len() as isize);
        match sysfs::write_value(node, next as i32) {
            Ok(()) => {
                self.set_status(format!("{label} -> {}", names[next as usize]));
                self.refresh();
            }
            Err(e) => self.set_status(format!("{e:#}")),
        }
    }

    fn toggle_gpu_boost(&mut self) {
        let next = i32::from(self.hw.gpu_boost.unwrap_or(0) == 0);
        match sysfs::write_value(sysfs::GPU_BOOST, next) {
            Ok(()) => {
                self.set_status(format!("GPU boost -> {}", sysfs::gpu_boost_name(Some(next))));
                self.refresh();
            }
            Err(e) => self.set_status(format!("{e:#}")),
        }
    }

    /// Left/Right (and Enter on a cycling item) move the selected value.
    fn step_selected(&mut self, delta: isize) {
        match self.selected_item() {
            Item::FanMode => self.cycle(sysfs::FAN_MODE, self.hw.fan_mode, &sysfs::FAN_MODES, delta, "Fan mode"),
            Item::ChargeMode => self.cycle(
                sysfs::CHARGE_MODE,
                self.hw.charge_mode,
                &sysfs::CHARGE_MODES,
                delta,
                "Charge mode",
            ),
            Item::GpuBoost => self.toggle_gpu_boost(),
            _ => {}
        }
    }

    fn refresh_with_status(&mut self) {
        self.refresh();
        self.set_status("Refreshed values");
    }

    fn open_fan_curve_editor(&mut self) {
        self.focus = Focus::FanCurveList;
        if self.fan_curve.is_none() {
            self.reload_fan_curve();
        }
    }

    // --- Input ---

    pub fn handle_key(&mut self, key: KeyEvent) -> Flow {
        // Raw mode delivers Ctrl-C as a key event rather than a signal, so it
        // has to be handled explicitly or the TUI cannot be interrupted.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Flow::Exit;
        }

        match self.focus {
            Focus::Editing => self.key_editing(key.code),
            Focus::FanCurveList => self.key_fan_curve(key.code),
            Focus::Normal => return self.key_normal(key.code),
        }
        Flow::Continue
    }

    fn key_editing(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.cancel_edit(),
            KeyCode::Enter => self.apply_edit(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            // Only digits: every editable node takes a non-negative number.
            KeyCode::Char(c) if c.is_ascii_digit() => self.input.push(c),
            _ => {}
        }
    }

    fn key_fan_curve(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.focus = Focus::Normal,
            KeyCode::Up => self.curve_row = self.curve_row.saturating_sub(1),
            KeyCode::Down => self.curve_row = (self.curve_row + 1).min(sysfs::FAN_CURVE_POINTS - 1),
            KeyCode::Left => self.curve_column = CurveColumn::Temp,
            KeyCode::Right => self.curve_column = CurveColumn::Speed,
            KeyCode::Enter => {
                if let Some(&(temp, speed)) = self.fan_curve.as_ref().and_then(|c| c.get(self.curve_row)) {
                    let seed = match self.curve_column {
                        CurveColumn::Temp => temp,
                        CurveColumn::Speed => speed,
                    };
                    self.start_edit(EditTarget::FanCurve(self.curve_row, self.curve_column), Some(seed));
                }
            }
            _ => {}
        }
    }

    fn key_normal(&mut self, code: KeyCode) -> Flow {
        match code {
            KeyCode::Char('q') => return Flow::Exit,
            KeyCode::Char('r') => self.refresh_with_status(),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Left => self.step_selected(-1),
            KeyCode::Right => self.step_selected(1),
            KeyCode::Enter | KeyCode::Char('e') => return self.activate(code == KeyCode::Enter),
            _ => {}
        }
        Flow::Continue
    }

    /// Enter acts on the selected item; `e` only opens editors, so that holding
    /// a value key cannot cycle a mode by accident.
    fn activate(&mut self, enter: bool) -> Flow {
        match self.selected_item() {
            Item::FanCustomSpeed => self.start_edit(EditTarget::FanCustomSpeed, self.hw.fan_custom_speed),
            Item::ChargeLimit => self.start_edit(EditTarget::ChargeLimit, self.hw.charge_limit),
            Item::FanCurveEdit => self.open_fan_curve_editor(),
            Item::Refresh if enter => self.refresh_with_status(),
            Item::Quit if enter => return Flow::Exit,
            _ if enter => self.step_selected(1),
            _ => {}
        }
        Flow::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app with the hardware snapshot blanked, so results do not depend on
    /// whether the machine running the tests has the driver loaded.
    fn app() -> App {
        let mut app = App::new(Config::default());
        app.hw = HwState::default();
        app.fan_curve = None;
        app
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut app = app();
        app.move_selection(-1);
        assert_eq!(app.selected_item(), Item::Quit);
        app.move_selection(1);
        assert_eq!(app.selected_item(), Item::FanMode);
        app.move_selection(Item::ALL.len() as isize + 1);
        assert_eq!(app.selected_item(), Item::FanCustomSpeed);
    }

    #[test]
    fn every_item_has_a_title_and_hint() {
        for item in Item::ALL {
            assert!(!item.title().is_empty());
            assert!(!item.hint().is_empty());
        }
    }

    #[test]
    fn q_and_ctrl_c_exit_but_other_keys_do_not() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Char('x'))), Flow::Continue);
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Flow::Exit);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Flow::Exit
        );
    }

    #[test]
    fn editing_accepts_digits_only_and_esc_restores_focus() {
        let mut app = app();
        app.selected = Item::ALL.iter().position(|i| *i == Item::FanCustomSpeed).unwrap();
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Editing);

        for code in [KeyCode::Char('1'), KeyCode::Char('a'), KeyCode::Char('2')] {
            app.handle_key(key(code));
        }
        assert_eq!(app.input, "12");
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "1");

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Normal);
        assert!(app.input.is_empty());
        assert_eq!(app.editing, None);
    }

    #[test]
    fn cancelling_a_curve_edit_returns_to_the_table() {
        let mut app = app();
        app.fan_curve = Some(vec![(30, 40); sysfs::FAN_CURVE_POINTS]);
        app.open_fan_curve_editor();
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Right));
        assert_eq!((app.curve_row, app.curve_column), (1, CurveColumn::Speed));

        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.editing, Some(EditTarget::FanCurve(1, CurveColumn::Speed)));
        assert_eq!(app.input, "40");

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::FanCurveList);
    }

    #[test]
    fn curve_navigation_stays_in_bounds() {
        let mut app = app();
        app.fan_curve = Some(vec![(30, 40); sysfs::FAN_CURVE_POINTS]);
        app.open_fan_curve_editor();
        for _ in 0..sysfs::FAN_CURVE_POINTS + 5 {
            app.handle_key(key(KeyCode::Down));
        }
        assert_eq!(app.curve_row, sysfs::FAN_CURVE_POINTS - 1);
        for _ in 0..sysfs::FAN_CURVE_POINTS + 5 {
            app.handle_key(key(KeyCode::Up));
        }
        assert_eq!(app.curve_row, 0);
    }

    #[test]
    fn enter_on_an_unloaded_curve_does_not_open_an_editor() {
        let mut app = app();
        app.fan_curve = None;
        app.focus = Focus::FanCurveList;
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::FanCurveList);
        assert_eq!(app.editing, None);
    }

    #[test]
    fn edit_prompts_name_what_is_being_edited() {
        assert_eq!(EditTarget::ChargeLimit.prompt(), "Enter charge limit");
        assert_eq!(EditTarget::FanCurve(3, CurveColumn::Temp).prompt(), "Enter temp for idx 3");
        assert_eq!(EditTarget::FanCurve(3, CurveColumn::Speed).prompt(), "Enter speed for idx 3");
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
}
