//! State and input handling for the interactive TUI.

use std::{ops::RangeInclusive, time::Instant};

use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    config::{self, Config, Profile},
    history::History,
    notify::Notifier,
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
    Profiles,
    Refresh,
    Quit,
}

impl Item {
    pub const ALL: [Self; 11] = [
        Self::FanMode,
        Self::FanCustomSpeed,
        Self::ChargeMode,
        Self::ChargeLimit,
        Self::GpuBoost,
        Self::FanCurveView,
        Self::FanCurveEdit,
        Self::History,
        Self::Profiles,
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
            Self::Profiles => "Profiles",
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
            Self::Profiles => "Press Enter to apply, save, update, or delete saved profiles",
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
    /// Name for a profile saved from the current hardware state.
    NewProfileName,
}

impl EditTarget {
    /// The values the device accepts for this field, for the numeric targets.
    fn range(self) -> Option<RangeInclusive<i32>> {
        Some(match self {
            Self::FanCustomSpeed => sysfs::FAN_SPEED_RANGE,
            Self::ChargeLimit => sysfs::CHARGE_LIMIT_RANGE,
            Self::FanCurve(_, CurveColumn::Temp) => sysfs::CURVE_TEMP_RANGE,
            Self::FanCurve(_, CurveColumn::Speed) => sysfs::CURVE_SPEED_RANGE,
            Self::NewProfileName => return None,
        })
    }

    /// Whether this field takes a number, which is also what the editor accepts
    /// keystrokes for.
    pub fn is_numeric(self) -> bool {
        self.range().is_some()
    }

    pub fn prompt(self) -> String {
        match self {
            Self::FanCustomSpeed => "Enter fan custom speed".to_string(),
            Self::ChargeLimit => "Enter charge limit".to_string(),
            Self::FanCurve(index, CurveColumn::Temp) => format!("Enter temp for idx {index}"),
            Self::FanCurve(index, CurveColumn::Speed) => format!("Enter speed for idx {index}"),
            Self::NewProfileName => "Name for the new profile".to_string(),
        }
    }

    /// What the editor tells the user is acceptable.
    pub fn hint(self) -> String {
        match self.range() {
            Some(range) => format!("Allowed: {}..{}", range.start(), range.end()),
            None => "Any name; Enter saves, Esc cancels".to_string(),
        }
    }
}

/// Which part of the UI has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Normal,
    Editing,
    FanCurveList,
    ProfileList,
    /// A yes/no question is on screen; nothing else takes keys until answered.
    Confirm,
}

/// A destructive action held until the user confirms it.
pub struct Confirm {
    pub message: String,
    action: ConfirmAction,
}

enum ConfirmAction {
    DeleteProfile(String),
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

    /// Saved profiles, sorted by name so the list order is stable.
    pub profiles: Vec<(String, Profile)>,
    pub profile_row: usize,
    pub confirm: Option<Confirm>,

    pub fans: Vec<Fan>,
    pub temps: Temps,
    pub history: History,
    pub config: Config,
    pub last_refresh: Instant,

    sensors: Sensors,
    notifier: Notifier,
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
            profiles: Vec::new(),
            profile_row: 0,
            confirm: None,
            fans: Vec::new(),
            temps: Temps::default(),
            history: History::new(config.history_length),
            last_refresh: Instant::now(),
            sensors: Sensors::new(),
            notifier: Notifier::new(&config.notifications),
            config,
        };
        app.reload_profiles();
        app.refresh();
        app
    }

    /// Re-reads the profiles file. Failures leave the list empty rather than
    /// taking down the UI.
    fn reload_profiles(&mut self) {
        match config::load_profiles() {
            Ok(profiles) => {
                let mut profiles: Vec<(String, Profile)> = profiles.into_iter().collect();
                profiles.sort_by(|a, b| a.0.cmp(&b.0));
                self.profiles = profiles;
            }
            Err(e) => {
                self.profiles.clear();
                self.set_status(format!("Could not load profiles: {e:#}"));
            }
        }
        self.profile_row = self.profile_row.min(self.profiles.len().saturating_sub(1));
    }

    pub fn selected_profile(&self) -> Option<&(String, Profile)> {
        self.profiles.get(self.profile_row)
    }

    /// Applies the selected profile, and follows it through to the system power
    /// profile when the profile maps to one.
    fn apply_selected_profile(&mut self) {
        let Some((name, profile)) = self.selected_profile() else {
            self.set_status("No profiles saved yet - press s to save the current settings");
            return;
        };
        let (name, profile) = (name.clone(), profile.clone());

        match profile.apply() {
            Ok(()) => {
                let mut message = format!("Applied profile '{name}'");
                if let Some(power_profile) = &profile.ppd_profile {
                    match crate::ppd::set(power_profile) {
                        Ok(()) => message.push_str(&format!(", power profile -> {power_profile}")),
                        Err(e) => message.push_str(&format!(" (power profile unchanged: {e})")),
                    }
                }
                self.set_status(message);
                self.refresh();
            }
            Err(e) => self.set_status(format!("{e:#}")),
        }
    }

    /// Saves the current hardware state under `name`, replacing any profile
    /// already using it.
    fn save_profile(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.set_status("Profile name cannot be empty");
            return;
        }
        let mut profiles: std::collections::HashMap<String, Profile> = self.profiles.iter().cloned().collect();
        // Keep the power profile mapping when overwriting, since it is not
        // something the hardware can report back.
        let mut profile = Profile::from_hardware();
        if let Some(existing) = profiles.get(name) {
            profile.ppd_profile = existing.ppd_profile.clone();
        }
        let replaced = profiles.insert(name.to_string(), profile).is_some();

        match config::save_profiles(&profiles) {
            Ok(()) => {
                self.set_status(format!("{} profile '{name}'", if replaced { "Updated" } else { "Saved" }));
                self.reload_profiles();
                if let Some(row) = self.profiles.iter().position(|(saved, _)| saved == name) {
                    self.profile_row = row;
                }
            }
            Err(e) => self.set_status(format!("{e:#}")),
        }
    }

    fn delete_profile(&mut self, name: &str) {
        let mut profiles: std::collections::HashMap<String, Profile> = self.profiles.iter().cloned().collect();
        profiles.remove(name);
        match config::save_profiles(&profiles) {
            Ok(()) => {
                self.set_status(format!("Deleted profile '{name}'"));
                self.reload_profiles();
            }
            Err(e) => self.set_status(format!("{e:#}")),
        }
    }

    fn ask(&mut self, message: impl Into<String>, action: ConfirmAction) {
        self.confirm = Some(Confirm { message: message.into(), action });
        self.focus = Focus::Confirm;
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
        self.notifier.check(self.temps);
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
            Some(EditTarget::NewProfileName) => Focus::ProfileList,
            _ => Focus::Normal,
        };
        self.editing = None;
        self.input.clear();
    }

    /// Writes the typed value to the device, keeping the popup open on error so
    /// the entry can be corrected.
    fn apply_edit(&mut self) {
        let Some(target) = self.editing else { return };
        if target == EditTarget::NewProfileName {
            let name = self.input.trim().to_string();
            if name.is_empty() {
                self.set_status("Profile name cannot be empty");
                return;
            }
            self.cancel_edit();
            self.save_profile(&name);
            return;
        }
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
            // Handled above; it does not take a number.
            EditTarget::NewProfileName => return,
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

    /// Rewrites one curve point, keeping the column that was not edited and
    /// refusing edits that would put the curve out of order.
    fn write_curve_value(&self, index: usize, column: CurveColumn, value: i32) -> Result<()> {
        let curve = self.fan_curve.as_ref().ok_or_else(|| anyhow!("Curve not loaded"))?;
        let &(temp, speed) = curve.get(index).ok_or_else(|| anyhow!("Curve not loaded"))?;
        let (temp, speed) = match column {
            CurveColumn::Temp => (value, speed),
            CurveColumn::Speed => (temp, value),
        };
        sysfs::validate_curve_point_in(curve, index, temp, speed)?;
        sysfs::write_fan_curve_point(index, temp, speed)
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
            Focus::ProfileList => self.key_profiles(key.code),
            Focus::Confirm => self.key_confirm(key.code),
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
            // Numeric fields take digits only; the profile name takes text.
            KeyCode::Char(c) if self.editing.is_some_and(EditTarget::is_numeric) => {
                if c.is_ascii_digit() {
                    self.input.push(c);
                }
            }
            KeyCode::Char(c) if !c.is_control() => self.input.push(c),
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

    fn key_profiles(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.focus = Focus::Normal,
            KeyCode::Up => self.profile_row = self.profile_row.saturating_sub(1),
            KeyCode::Down => {
                self.profile_row = (self.profile_row + 1).min(self.profiles.len().saturating_sub(1));
            }
            KeyCode::Enter => self.apply_selected_profile(),
            KeyCode::Char('s') => self.start_edit(EditTarget::NewProfileName, None),
            KeyCode::Char('u') => match self.selected_profile() {
                Some((name, _)) => {
                    let name = name.clone();
                    self.save_profile(&name);
                }
                None => self.set_status("No profile selected"),
            },
            KeyCode::Char('d') => match self.selected_profile() {
                Some((name, _)) => {
                    let name = name.clone();
                    self.ask(format!("Delete profile '{name}'? [y/N]"), ConfirmAction::DeleteProfile(name));
                }
                None => self.set_status("No profile selected"),
            },
            _ => {}
        }
    }

    fn key_confirm(&mut self, code: KeyCode) {
        let confirmed = matches!(code, KeyCode::Char('y' | 'Y'));
        let dismissed = confirmed || matches!(code, KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter);
        if !dismissed {
            return;
        }

        let pending = self.confirm.take();
        self.focus = Focus::ProfileList;
        let Some(confirm) = pending else { return };
        if !confirmed {
            self.set_status("Cancelled");
            return;
        }
        match confirm.action {
            ConfirmAction::DeleteProfile(name) => self.delete_profile(&name),
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
            Item::Profiles => {
                self.reload_profiles();
                self.focus = Focus::ProfileList;
            }
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
        // Whatever this machine has saved is not this test's business.
        app.profiles.clear();
        app.profile_row = 0;
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
    fn profile_list_navigation_and_prompts() {
        let mut app = app();
        app.selected = Item::ALL.iter().position(|i| *i == Item::Profiles).unwrap();
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::ProfileList);

        // Entering the list re-reads the profiles file, so stand in a known set
        // only once that has happened.
        app.profiles = vec![
            ("balanced".to_string(), Profile::default()),
            ("gaming".to_string(), Profile::default()),
        ];
        app.profile_row = 0;

        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected_profile().unwrap().0, "gaming");
        // Selection stops at the end rather than wrapping or overflowing.
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.profile_row, 1);
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.profile_row, 0);

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Normal);
    }

    #[test]
    fn saving_a_profile_prompts_for_a_name_as_free_text() {
        let mut app = app();
        app.focus = Focus::ProfileList;
        app.handle_key(key(KeyCode::Char('s')));
        assert_eq!(app.editing, Some(EditTarget::NewProfileName));
        assert!(app.input.is_empty(), "the name field starts empty");

        for c in "my rig 2".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "my rig 2", "text targets accept letters and spaces");

        // Escaping returns to the list, not the main menu.
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::ProfileList);
        assert_eq!(app.editing, None);
    }

    #[test]
    fn deleting_a_profile_asks_first() {
        let mut app = app();
        app.profiles = vec![("gaming".to_string(), Profile::default())];
        app.focus = Focus::ProfileList;

        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.focus, Focus::Confirm);
        assert!(app.confirm.as_ref().unwrap().message.contains("gaming"));

        // Anything other than an answer leaves the question up.
        app.handle_key(key(KeyCode::Char('x')));
        assert_eq!(app.focus, Focus::Confirm);

        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focus, Focus::ProfileList);
        assert!(app.confirm.is_none());
        assert_eq!(app.profiles.len(), 1, "declining must not delete anything");
    }

    #[test]
    fn deleting_with_no_profiles_selected_is_harmless() {
        let mut app = app();
        app.profiles.clear();
        app.focus = Focus::ProfileList;
        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Char('u')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::ProfileList);
        assert!(app.confirm.is_none());
    }

    #[test]
    fn numeric_fields_still_reject_letters() {
        let mut app = app();
        app.selected = Item::ALL.iter().position(|i| *i == Item::ChargeLimit).unwrap();
        app.handle_key(key(KeyCode::Enter));
        for c in "8a0".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "80");
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
