# gigabytectl

A simple Rust-based TUI app for controlling laptops using the `gigabyte-laptop-wmi` kernel module.

---

## 📦 Installation

### Method 1: Using Cargo

```bash
cargo install gigabytectl
```

> ⚠️ Cargo installs binaries to `~/.cargo/bin`.  
> Make sure it’s in your `PATH`:
>
> ```bash
> export PATH="$HOME/.cargo/bin:$PATH"
> ```

---

### Method 2: Prebuilt Binary (GitHub Releases)

Download the latest release, then:

```bash
tar xf gigabytectl-*.tar.gz
chmod +x gigabytectl
```

#### ▶ Run directly

```bash
sudo ./gigabytectl
```

---

### 📌 Optional: Install system-wide

```bash
sudo install -Dm755 gigabytectl /usr/local/bin/gigabytectl
```

Then you can run:

```bash
sudo gigabytectl
```

---

## ⚠️ Permissions

This tool requires root privileges to access:

```
/sys/devices/platform/aorus_laptop/
```

So it must be run with:

```bash
sudo gigabytectl
```

---

## 🧹 Uninstall

If installed system-wide:

```bash
sudo rm /usr/local/bin/gigabytectl
```

If installed via Cargo:

```bash
cargo uninstall gigabytectl
```


