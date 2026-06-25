# gigabytectl

A simple Rust-based TUI tool for controlling laptops using the `gigabyte-laptop-wmi` kernel module.

## 📸 Preview

![preview](assets/preview.gif)

See More:
[Assets](assets)

## ✨ Features

- View fan speeds in real time
- Control fan speed via a simple TUI
- Scriptable headless/CLI mode for automation
- Lightweight and fast (written in Rust)
- Direct integration with `gigabyte-laptop-wmi`
- No background services or daemons required
- Works directly with `/sys` interfaces
- Minimal dependencies
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
```

Run `gigabytectl --help` or `gigabytectl <command> --help` for the full list of commands and options. CLI subcommands require root and exit with a clear error (rather than an interactive prompt) if not run with `sudo`, so they are safe to use in scripts.

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