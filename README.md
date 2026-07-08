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
- No background services or daemons required
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

sudo gigabytectl battery-cycle
sudo gigabytectl fans                     # live fan RPM readings

sudo gigabytectl fan-curve get            # all 15 points (index temp speed)
sudo gigabytectl fan-curve get 3          # single point
sudo gigabytectl fan-curve set 3 40 120   # index temp speed

gigabytectl monitor                       # live temps + fan RPM (Ctrl-C to stop)
gigabytectl monitor --interval 2 --json   # custom interval, JSON per line
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

[quiet]
fan_mode = "silent"
charge_mode = "normal"     # normal|custom
fan_custom_speed = 30      # 25..100, step 5
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