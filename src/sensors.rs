//! Fan and temperature readings from `/sys/class/hwmon` (plus `nvidia-smi` as a
//! fallback for the proprietary NVIDIA driver).
//!
//! Device paths are resolved once and reused: the TUI samples these several
//! times per second, and rescanning `/sys/class/hwmon` on every tick is pure
//! overhead.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::system::command_stdout;

const HWMON_ROOT: &str = "/sys/class/hwmon";
/// `name` of the hwmon device registered by the laptop WMI driver.
const FAN_HWMON: [&str; 1] = ["aorus_laptop"];
const CPU_HWMON: [&str; 3] = ["coretemp", "k10temp", "zenpower"];
const GPU_HWMON: [&str; 2] = ["amdgpu", "nouveau"];
/// `nvidia-smi` costs hundreds of milliseconds per call, so its result is reused
/// for this long rather than blocking every refresh.
const NVIDIA_CACHE_TTL: Duration = Duration::from_secs(2);

/// The driver's hwmon fan channels are fixed: 1 is the CPU fan, 2 the GPU fan,
/// and 3 and 4 are extra fans only some models are fitted with.
const FAN_CHANNEL_NAMES: [&str; 2] = ["CPU fan", "GPU fan"];
/// Channels that always exist, so a reading of `0` means "stopped" rather than
/// "not fitted" and is worth showing.
const ALWAYS_PRESENT_FANS: u32 = FAN_CHANNEL_NAMES.len() as u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fan {
    /// hwmon channel number, starting at 1.
    pub channel: u32,
    pub name: String,
    pub rpm: u32,
    /// Raw PWM duty for this fan, for the channels that report one. The driver
    /// passes the embedded controller's value through unscaled and its range is
    /// undocumented, so it is shown as-is rather than as a percentage.
    pub pwm: Option<u32>,
}

impl Fan {
    /// `"5357 RPM (PWM 60)"`, or just the RPM when the channel reports no duty.
    pub fn reading(&self) -> String {
        match self.pwm {
            Some(pwm) => format!("{} RPM (PWM {pwm})", self.rpm),
            None => format!("{} RPM", self.rpm),
        }
    }

    fn name_for(channel: u32) -> String {
        // hwmon channels are 1-based; anything outside the known set keeps a
        // generic name.
        (channel as usize)
            .checked_sub(1)
            .and_then(|index| FAN_CHANNEL_NAMES.get(index))
            .map_or_else(|| format!("Fan {channel}"), |name| (*name).to_string())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Temps {
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
}

/// Where the GPU temperature comes from on this machine.
#[derive(Debug)]
enum GpuSource {
    Hwmon(PathBuf),
    NvidiaSmi,
    Unavailable,
}

/// Resolved sensor locations for this machine.
#[derive(Debug)]
pub struct Sensors {
    fans: Option<PathBuf>,
    cpu: Option<PathBuf>,
    gpu: GpuSource,
    nvidia_cache: Option<(Instant, f32)>,
}

impl Sensors {
    pub fn new() -> Self {
        Self {
            fans: find_hwmon(&FAN_HWMON),
            cpu: find_hwmon(&CPU_HWMON).map(|dir| dir.join("temp1_input")),
            gpu: match find_hwmon(&GPU_HWMON) {
                Some(dir) => GpuSource::Hwmon(dir.join("temp1_input")),
                // Assume `nvidia-smi` until a call proves otherwise.
                None => GpuSource::NvidiaSmi,
            },
            nvidia_cache: None,
        }
    }

    /// Live fan readings, ordered by channel.
    ///
    /// The CPU and GPU fans are always reported, so a stopped fan reads `0`
    /// rather than vanishing from the list. The extra channels only exist on
    /// some models, so those are dropped when they read `0`.
    pub fn read_fans(&self) -> Vec<Fan> {
        let Some(dir) = &self.fans else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };

        let mut fans: Vec<Fan> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let channel = fan_input_index(path.file_name()?.to_str()?)?;
                let rpm: u32 = fs::read_to_string(&path).ok()?.trim().parse().ok()?;
                if rpm == 0 && channel > ALWAYS_PRESENT_FANS {
                    return None;
                }
                Some(Fan {
                    channel,
                    name: Fan::name_for(channel),
                    rpm,
                    pwm: read_u32(&dir.join(format!("pwm{channel}"))),
                })
            })
            .collect();

        // read_dir order is arbitrary; sort so the list does not jump around.
        fans.sort_by_key(|fan| fan.channel);
        fans
    }

    pub fn read_temps(&mut self) -> Temps {
        Temps {
            cpu: self.cpu.as_deref().and_then(read_hwmon_temp),
            gpu: self.read_gpu_temp(),
        }
    }

    fn read_gpu_temp(&mut self) -> Option<f32> {
        match &self.gpu {
            GpuSource::Hwmon(path) => read_hwmon_temp(path),
            GpuSource::Unavailable => None,
            GpuSource::NvidiaSmi => {
                if let Some((at, temp)) = self.nvidia_cache
                    && at.elapsed() < NVIDIA_CACHE_TTL
                {
                    return Some(temp);
                }
                match nvidia_temp() {
                    Some(temp) => {
                        self.nvidia_cache = Some((Instant::now(), temp));
                        Some(temp)
                    }
                    // No NVIDIA driver here: stop paying for the probe.
                    None => {
                        self.gpu = GpuSource::Unavailable;
                        None
                    }
                }
            }
        }
    }
}

impl Default for Sensors {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the first `/sys/class/hwmon` device whose `name` is in `names`.
fn find_hwmon(names: &[&str]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(HWMON_ROOT).ok()?.flatten().map(|e| e.path()).collect();
    dirs.sort();
    dirs.into_iter()
        .find(|dir| fs::read_to_string(dir.join("name")).is_ok_and(|name| names.contains(&name.trim())))
}

/// Reads a `tempN_input` node, converting millidegrees to degrees Celsius.
fn read_hwmon_temp(path: &Path) -> Option<f32> {
    let milli: i32 = fs::read_to_string(path).ok()?.trim().parse().ok()?;
    Some(milli as f32 / 1000.0)
}

/// Reads an integer hwmon node, or `None` if the channel does not exist.
fn read_u32(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Extracts `N` from a `fanN_input` file name.
fn fan_input_index(file_name: &str) -> Option<u32> {
    file_name.strip_prefix("fan")?.strip_suffix("_input")?.parse().ok()
}

/// Best-effort GPU temperature via the NVIDIA proprietary driver.
fn nvidia_temp() -> Option<f32> {
    let out = command_stdout("nvidia-smi", &["--query-gpu=temperature.gpu", "--format=csv,noheader,nounits"])?;
    out.lines().next()?.trim().parse().ok()
}

#[cfg(test)]
impl Fan {
    /// Convenience constructor for tests.
    pub fn sample(channel: u32, rpm: u32) -> Self {
        Self { channel, name: Self::name_for(channel), rpm, pwm: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_input_files_are_recognised_by_index() {
        assert_eq!(fan_input_index("fan1_input"), Some(1));
        assert_eq!(fan_input_index("fan12_input"), Some(12));
        assert_eq!(fan_input_index("fan1_label"), None);
        assert_eq!(fan_input_index("temp1_input"), None);
        assert_eq!(fan_input_index("fan_input"), None);
    }

    #[test]
    fn probing_never_panics_on_machines_without_the_driver() {
        let mut sensors = Sensors::new();
        let _ = sensors.read_fans();
        let _ = sensors.read_temps();
    }

    #[test]
    fn fan_channels_carry_the_names_the_driver_documents() {
        assert_eq!(Fan::name_for(1), "CPU fan");
        assert_eq!(Fan::name_for(2), "GPU fan");
        assert_eq!(Fan::name_for(3), "Fan 3");
        assert_eq!(Fan::name_for(4), "Fan 4");
        // Never panics on a channel number the kernel should not produce.
        assert_eq!(Fan::name_for(0), "Fan 0");
    }
}
