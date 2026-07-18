# gigabytectl

A simple Rust-based TUI tool for controlling laptops using the `gigabyte-laptop-wmi` kernel module.

## 📸 Preview

![preview](assets/preview.gif)

See More:
[Assets](assets)

## ✨ Features

- View fan speeds and CPU/GPU temperatures in real time
- Control fan speed via a simple TUI
- Live history graph of temperatures and fan RPM
- Save and apply configuration profiles
- Scriptable headless/CLI mode for automation, including a `monitor` mode
- Shell completions for bash, zsh, and fish
- Configurable defaults (refresh interval, temperature units)
- Lightweight and fast (written in Rust)
- Direct integration with `gigabyte-laptop-wmi`
- Optional two-way sync with `power-profiles-daemon`
- Works directly with `/sys` interfaces
- Keyboard-driven interface

## ⬇️ Installation

### Method 1: 🦀 Using Cargo

Since `gigabytectl` needs root, and `sudo` does not use your `PATH`, installing with a plain `cargo install gigabytectl` (which installs to `~/.cargo/bin`) means `sudo gigabytectl` will fail with "command not found". Install straight into `/usr/local`, which is on root's `PATH`, using `--root`:

```bash
sudo cargo install gigabytectl --root /usr/local
```

Now `sudo gigabytectl` will work correctly.


### Method 2: 📦 Prebuilt Binary (GitHub Releases)

Download the latest release, then:

```bash
tar xf gigabytectl-*.tar.gz
chmod +x gigabytectl
```

#### ▶ Run directly

```bash
sudo ./gigabytectl
```


#### ▶ Optional: Install system-wide

```bash
sudo install -Dm755 gigabytectl /usr/local/bin/gigabytectl
```

Then you can run:

```bash
sudo gigabytectl
```


## ⚠️ Permissions

This tool requires root privileges to access `/sys`.

If you are not running with `sudo`, the app will prompt you on startup:

- Press `y` or `Enter` → continue with root privileges (`sudo`)
- Press `n` → exit

You can also run it directly with `sudo`:

```bash
sudo gigabytectl
``` 

## ⚙️ Headless / CLI Mode

Running `gigabytectl` with no arguments launches the TUI. Passing a subcommand instead runs a one-shot, scriptable command and exits — no TUI required.

```bash
sudo gigabytectl status                  # human-readable summary
sudo gigabytectl status --json           # JSON output

sudo gigabytectl fan-mode get
sudo gigabytectl fan-mode set gaming      # normal|silent|gaming|custom|auto|fixed

sudo gigabytectl fan-speed get
sudo gigabytectl fan-speed set 50         # 25..100, step 5

sudo gigabytectl charge-mode get
sudo gigabytectl charge-mode set custom   # normal|custom

sudo gigabytectl charge-limit get
sudo gigabytectl charge-limit set 80      # 60..100

sudo gigabytectl gpu-boost get
sudo gigabytectl gpu-boost set on         # on|off

sudo gigabytectl battery-cycle            # show the battery cycle count
sudo gigabytectl light-sensor             # show light sensor data
sudo gigabytectl fan-pwm                  # show current CPU fan PWM
sudo gigabytectl fans                     # live fan RPM readings

sudo gigabytectl fan-curve get            # all 15 points (index temp speed)
sudo gigabytectl fan-curve get 3          # single point
sudo gigabytectl fan-curve set 3 40 120   # index temp speed

gigabytectl monitor                       # live temps + fan RPM (Ctrl-C to stop)
gigabytectl monitor --interval 2 --json   # custom interval, JSON per line

sudo gigabytectl sync --once              # apply profile mapped to current power profile
sudo gigabytectl sync                     # watch power-profiles-daemon and sync (see below)
```

`monitor`, `completions`, and `profile --list` are read-only and do not require root. The other subcommands read from or write to `/sys` and require `sudo`; they exit with a clear error (rather than an interactive prompt) if not run as root, so they are safe to use in scripts.

Run `gigabytectl --help` or `gigabytectl <command> --help` for the full list of commands and options.

## 🎚️ Profiles

Save a bundle of settings and reapply it later with a single command. Profiles live in `~/.config/gigabytectl/profiles.toml`.

```bash
sudo gigabytectl profile --save gaming    # snapshot current settings as "gaming"
gigabytectl profile --list                # list saved profiles
sudo gigabytectl profile gaming           # apply the "gaming" profile
```

You can also write profiles by hand. Every field is optional — a profile only changes what it specifies:

```toml
[gaming]
fan_mode = "gaming"        # normal|silent|gaming|custom|auto|fixed
charge_limit = 80          # 60..100
gpu_boost = "on"           # on|off
ppd_profile = "performance" # optional: power-profiles-daemon profile (see below)

[quiet]
fan_mode = "silent"
charge_mode = "normal"     # normal|custom
fan_custom_speed = 30      # 25..100, step 5
ppd_profile = "power-saver"
# Optional full 15-point fan curve as [temp, speed] pairs:
# fan_curve = [[0,0], [40,20], [50,40], [60,80], [70,120], [80,180],
#              [90,220], [100,255], [100,255], [100,255], [100,255],
#              [100,255], [100,255], [100,255], [100,255]]
```

## 🛠️ Configuration

Optional defaults live in `~/.config/gigabytectl/config.toml`. Missing or invalid files fall back to the defaults shown below:

```toml
refresh_interval_ms = 1000   # TUI auto-refresh and default monitor interval
units = "celsius"            # celsius|fahrenheit (temperature display)
history_length = 120         # samples kept in the TUI history graph
```

> When run under `sudo`, config is resolved from the invoking user's home (via `$SUDO_USER`), not root's, so `~/.config/gigabytectl` is the same whether or not you use `sudo`.

## 🔋 Power Profiles Daemon Sync

If you run [`power-profiles-daemon`](https://gitlab.freedesktop.org/upower/power-profiles-daemon) (the default on GNOME/KDE, driving the Balanced/Performance/Power Saver toggle), gigabytectl can keep the two in sync **both ways**.

Give a profile a `ppd_profile` field to link it to a PPD profile (`power-saver`, `balanced`, or `performance`). That one field defines the mapping in both directions:

```toml
[gaming]
fan_mode = "gaming"
gpu_boost = "on"
ppd_profile = "performance"

[balanced]
fan_mode = "normal"
ppd_profile = "balanced"

[quiet]
fan_mode = "silent"
ppd_profile = "power-saver"
```

- **gigabytectl → PPD:** `sudo gigabytectl profile gaming` applies the hardware settings **and** switches the system power profile to `performance`.
- **PPD → gigabytectl:** run the sync daemon so that flipping the power profile (from your desktop's quick settings, `powerprofilesctl`, etc.) automatically applies the matching gigabytectl profile:

```bash
sudo gigabytectl sync --once   # apply the profile mapped to the current power profile, then exit
sudo gigabytectl sync          # keep watching and apply on every change (Ctrl-C to stop)
```

To run the watcher automatically at boot, install the systemd service:

```bash
sudo gigabytectl install-service
```

This writes the unit (pointing `ExecStart` at your actual binary), enables and starts it, and copies your profiles to `/etc/gigabytectl/profiles.toml`.

> **Why the copy?** The service runs as `root` under systemd, where `$SUDO_USER` is unset and `$HOME` is `/root`, so it can't see profiles saved in your home directory. gigabytectl reads `~/.config/gigabytectl/profiles.toml` first and falls back to `/etc/gigabytectl/profiles.toml`, which is where the service looks. After changing profiles, re-run `sudo gigabytectl install-service` (or edit the `/etc` copy directly) to update what the service applies.

You can also install the unit by hand (adjust `ExecStart` if `gigabytectl` isn't in `/usr/local/bin`):

```bash
sudo install -Dm644 assets/gigabytectl-ppd-sync.service /etc/systemd/system/gigabytectl-ppd-sync.service
sudo systemctl enable --now gigabytectl-ppd-sync.service
```

> If `power-profiles-daemon` isn't installed, the `ppd_profile` field and `sync` command are simply inert — everything else works as before. Setting a power profile requires `busctl` (systemd); the watcher requires `gdbus` (glib2).

## ⌨️ Shell Completions

Generate completions for your shell and load them:

```bash
# bash
gigabytectl completions bash | sudo tee /etc/bash_completion.d/gigabytectl > /dev/null

# zsh
gigabytectl completions zsh > ~/.zsh/completions/_gigabytectl

# fish
gigabytectl completions fish > ~/.config/fish/completions/gigabytectl.fish
```

## 🧹 Uninstall

If installed system-wide:

```bash
sudo rm /usr/local/bin/gigabytectl
```

If installed via Cargo:

```bash
sudo cargo uninstall gigabytectl --root /usr/local
```


## ↻ Update

### Method 1: 🦀 Using Cargo

```bash
sudo cargo install gigabytectl --root /usr/local --force
```

### Method 2: 📦 Prebuilt Binary (GitHub Releases)

If you installed using a prebuilt binary, simply:

- [Uninstall](https://github.com/Code-Sapling/gigabytectl#-uninstall)
- [Reinstall](https://github.com/Code-Sapling/gigabytectl#method-2--prebuilt-binary-github-releases)

## 💻 Compatibility

Works on Gigabyte / AORUS laptops using the `gigabyte-laptop-wmi` kernel module.
> You need the `gigabyte-laptop-wmi` kernel module.

## 🤖 AI Usage

This project was built with the help of AI tools. AI was used for code generation and documentation. All final decisions and testing were handled by me.

## 💥 Issues

If you find any problems or bugs, feel free to open an issue. Feedback and improvements are always welcome.

## 📝 Notes

- After updating **gigabytectl**, regenerate your [shell completions](https://github.com/Code-Sapling/gigabytectl#%EF%B8%8F-shell-completions).

- After modifying `~/.config/gigabytectl/profiles.toml`, reinstall the systemd service and update `/etc/gigabytectl/profiles.toml`:
  ```bash
  sudo gigabytectl install-service
  ```

- Keep **gigabytectl** and **gigabyte-laptop-wmi** in sync. Whenever you update one, it's recommended to update the other to ensure compatibility.