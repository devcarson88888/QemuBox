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
- Connect to remote Linux hosts over SSH and manage libvirt VMs.

## Requirements

- Rust and Cargo (1.85 or newer).
- A system CJK font for Chinese interface text (Windows: SimHei or Dengxian;
  Linux/macOS: Noto Sans CJK or an equivalent installed font).
- QEMU installed separately. Either add `qemu-system-x86_64` to `PATH` or
  choose its executable in the app.
- For remote management, OpenSSH must be available on the local `PATH`, and
  the remote Linux host must have `virsh`/libvirt installed. Configure SSH
  public-key authentication and libvirt permissions for the remote user.
- A QEMU-supported disk image. QemuBox uses `virtio` for the configured disk.

QemuBox runs SSH in batch mode and never asks for or stores a password. Before
connecting in the app, connect once from a terminal with `ssh -p PORT USER@HOST`
to verify the host fingerprint and confirm that key-based authentication works.
Saved remote connection profiles contain only the display name, host, user,
and port; they are stored in `QemuBox/remote-servers.json` in the user
configuration directory.

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
