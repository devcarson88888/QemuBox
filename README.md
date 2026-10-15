# QemuBox

QemuBox is a Rust desktop application for configuring and managing QEMU virtual
machines. It provides a graphical interface for saving VM settings and
starting or stopping QEMU processes, with a workstation-style home screen and
virtual machine library.

## Features

- Configure the QEMU executable, memory, CPU count, disk image, and optional
  installation ISO.
- Save VM definitions between launches.
- Start and stop configured instances and view their running status.
- Choose between `qcow2` and `raw` virtual disk formats.
- Import a saved virtual machine configuration from a JSON file.

## Requirements

- Rust and Cargo (1.85 or newer).
- A system CJK font for Chinese interface text (Windows: SimHei or Dengxian;
  Linux/macOS: Noto Sans CJK or an equivalent installed font).
- QEMU installed separately. Either add `qemu-system-x86_64` to `PATH` or
  choose its executable in the app.
- A QEMU-supported disk image. QemuBox uses `virtio` for the configured disk.

## Run

```sh
cargo run
```

Build a release executable with:

```sh
cargo build --release
```

VM settings are stored as JSON in the operating system's user configuration
directory under `QemuBox/vms.json`.
