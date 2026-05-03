# gigabytectl

A simple Rust-based TUI tool for controlling laptops using the `gigabyte-laptop-wmi` kernel module.

## 📸 Preview

### Main Dashboard
![dashboard](assets/dashboard.png)

### Fan Curve Graph
![graph](assets/graph.png)

### Fan Curve Editor
![editor](assets/editor.png)

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

> Cargo installs binaries to `~/.cargo/bin`.  
> Make sure it’s in your `PATH`


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

This tool requires root privileges to access:

```
/sys/devices/platform/aorus_laptop/
AND
/sys/class/hwmon
```

So it must be run with:

```bash
sudo gigabytectl
```


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