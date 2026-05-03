# gigabytectl

A simple Rust-based TUI tool for controlling laptops using the `gigabyte-laptop-wmi` kernel module.

## 📸 Preview

![preview](assets/preview.gif)

See More:
[Assets](assets)

## ✨ Features

- View fan speeds in real time
- Control fan speed via a simple TUI
- Lightweight and fast (written in Rust)
- Direct integration with `gigabyte-laptop-wmi`
- No background services or daemons required
- Works directly with `/sys` interfaces
- Minimal dependencies
- Keyboard-driven interface

## ⬇️ Installation

### Method 1: 🦀 Using Cargo

```bash
cargo install gigabytectl
```

> If `gigabytectl` is not found after installation, make sure `~/.cargo/bin` is in your `PATH`.  
> See the [Cargo PATH Setup](https://github.com/Code-Sapling/gigabytectl#-cargo-path-setup) section below.


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

If you encounter issues (especially when installed using `cargo`), `sudo` may cause path issues. In that case, try running without `sudo` and then choose `y` when prompted, or simply press Enter.

## 🧹 Uninstall

If installed system-wide:

```bash
sudo rm /usr/local/bin/gigabytectl
```

If installed via Cargo:

```bash
cargo uninstall gigabytectl
```


## ↻ Update

### Method 1: 🦀 Using Cargo

```bash
cargo install gigabytectl --force
```

### Method 2: 📦 Prebuilt Binary (GitHub Releases)

If you installed using a prebuilt binary, simply:

- [Uninstall](https://github.com/Code-Sapling/gigabytectl#-uninstall)
- [Reinstall](https://github.com/Code-Sapling/gigabytectl#method-2--prebuilt-binary-github-releases)

## 💻 Compatibility

Works on Gigabyte / AORUS laptops using the `gigabyte-laptop-wmi` kernel module.
> You need the `gigabyte-laptop-wmi` kernel module.

## 🦀 Cargo PATH Setup

Cargo installs binaries to:

```
~/.cargo/bin
```

If this directory is not in your `PATH`, you won’t be able to run installed binaries.

To add it, run:

```bash
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.bashrc
```
Then restart your terminal

## 🤖 AI Usage

This project was built with the help of AI tools. AI was used for code generation and documentation. All final decisions and testing were handled by me.

## 💥 Issues

If you find any problems or bugs, feel free to open an issue. Feedback and improvements are always welcome.