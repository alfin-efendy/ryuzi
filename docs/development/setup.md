# Pre-Development Setup

This guide covers everything needed to build the monorepo from source across all platforms. The repo contains two stacks:

- **JS/TS** (CLI, web, packages) — requires **Bun**
- **Rust/Tauri** (Cockpit desktop app) — requires **Rust** + a C++ toolchain + **WebView**

If you only work on the JS side, you only need Bun. If you touch Cockpit (`apps/cockpit`), you need the full stack below.

---

## macOS

### 1. Xcode Command Line Tools

Provides `clang`, `git`, and the macOS SDK — required by Rust's linker.

```sh
xcode-select --install
```

### 2. Bun

```sh
curl -fsSL https://bun.sh/install | bash
```

### 3. Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

The default target (`aarch64-apple-darwin` on Apple Silicon, `x86_64-apple-darwin` on Intel) is correct — no extra steps needed.

### 4. Verify

```sh
make doctor
```

Expected output:

```
bun:   1.x.x
cargo: cargo 1.x.x
rustc: rustc 1.x.x
tauri: tauri-cli x.x.x
```

---

## Linux (Debian / Ubuntu)

### 1. System packages

Tauri needs WebKitGTK and several other libraries:

```sh
sudo apt update
sudo apt install -y \
  build-essential \
  curl \
  wget \
  file \
  libssl-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libwebkit2gtk-4.1-dev \
  libxdo-dev \
  libsoup-3.0-dev \
  javascriptcoregtk-4.1
```

> **Fedora / RHEL:** Replace the `apt` block with the equivalent `dnf install` packages: `webkit2gtk4.1-devel`, `openssl-devel`, `gtk3-devel`, `librsvg2-devel`, `libappindicator-gtk3-devel`.

### 2. Bun

```sh
curl -fsSL https://bun.sh/install | bash
source "$HOME/.bashrc"   # or ~/.zshrc
```

### 3. Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### 4. Verify

```sh
make doctor
```

---

## Windows

Windows requires the most setup because Rust needs the **MSVC** toolchain (not the GNU/MinGW one), which in turn needs **Visual Studio Build Tools** and the **Windows SDK**.

> **Important:** The default Rust installer on Windows may select the GNU toolchain. Follow the steps below exactly to avoid linker errors.

### 1. Git

Download and install from <https://git-scm.com/download/win>. Accept the default options.

### 2. Bun

Open **PowerShell** and run:

```powershell
powershell -c "irm bun.sh/install.ps1 | iex"
```

Restart the terminal after installation.

### 3. Visual Studio Build Tools (with C++ workload + Windows SDK)

Install **Visual Studio Build Tools** (or the full Visual Studio IDE):

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools
```

When the installer opens, select the **"Desktop development with C++"** workload. This installs the MSVC compiler, linker (`link.exe`), and **Windows 11 SDK** in one step.

> **Already have Visual Studio installed?** Open the **Visual Studio Installer** → **Modify** → enable "Desktop development with C++" → ensure "Windows 11 SDK" is checked under Individual components → **Modify**.

Verify that `link.exe` is available. From a **Developer Command Prompt for VS**:

```cmd
where link.exe
```

It should print something like:
```
C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.x.x\bin\Hostx64\x64\link.exe
```

### 4. Rust (MSVC toolchain)

```powershell
winget install Rustlang.Rustup
```

Rustup's Windows installer defaults to the MSVC host. Confirm after installing:

```powershell
rustup show active-toolchain
# expected: stable-x86_64-pc-windows-msvc (default)
```

If it shows `windows-gnu` instead, switch it:

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc
rustup default stable-x86_64-pc-windows-msvc
```

> **Why not GNU?** The GNU toolchain (`x86_64-pc-windows-gnu`) requires MinGW binutils (`dlltool.exe`) which is a separate install and is not needed. Tauri's Windows support is built and tested against MSVC. Always use MSVC on Windows.

### 5. Verify

Open a **normal PowerShell** (not Developer Command Prompt — cargo finds the toolchain on its own):

```powershell
make doctor
```

Expected output:

```
bun:   1.x.x
cargo: cargo 1.x.x
rustc: rustc 1.x.x
tauri: tauri-cli x.x.x
```

---

## First-time setup (all platforms)

Once the toolchain is ready, from the repo root:

```sh
make setup   # bun install + cargo fetch
make cockpit # start Cockpit in dev mode (HMR)
```

`make setup` only needs to run once (and again after pulling major dependency changes).

---

## Troubleshooting

### `bun: command not found: tauri`

The JS dependencies are not installed. Run `bun install` from the repo root.

### `error calling dlltool 'dlltool.exe': program not found` (Windows)

Your Rust default is the GNU toolchain. Switch to MSVC:

```powershell
rustup default stable-x86_64-pc-windows-msvc
```

### `linker 'link.exe' not found` (Windows)

Visual Studio Build Tools are missing or the C++ workload was not selected. Rerun the VS Installer and enable **"Desktop development with C++"**.

### `cannot open input file 'kernel32.lib'` (Windows)

The **Windows SDK** is not installed. Open the VS Installer → Modify → Individual components → search for **"Windows 11 SDK"** → check it → Modify.

### WebKitGTK not found (Linux)

Run the system package install step again with `sudo apt install libwebkit2gtk-4.1-dev`.

<!-- ci filter probe: docs-only (throwaway) -->
