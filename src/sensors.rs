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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fan {
    pub name: String,
    pub rpm: u32,
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

    /// Live fan readings, ordered by fan number. Channels reporting `0` are
    /// omitted: the driver exposes a fixed set of inputs, not all of which are
    /// wired up on every model.
    pub fn read_fans(&self) -> Vec<Fan> {
        let Some(dir) = &self.fans else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };

        let mut fans: Vec<(u32, Fan)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let index = fan_input_index(path.file_name()?.to_str()?)?;
                let rpm: u32 = fs::read_to_string(&path).ok()?.trim().parse().ok()?;
                (rpm > 0).then(|| (index, Fan { name: format!("Fan {index}"), rpm }))
            })
            .collect();

        // read_dir order is arbitrary; sort so the list does not jump around.
        fans.sort_by_key(|(index, _)| *index);
        fans.into_iter().map(|(_, fan)| fan).collect()
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
}
