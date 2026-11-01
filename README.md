# Talyvor for desktop

Taly on the computer: a Talyvor icon in the macOS menu bar or the Windows tray. Press **⇧⌘Space** (Windows:
**Ctrl+Shift+Space**), or click the icon, and a small Taly window opens over whatever you are doing.

- **Ask Taly**: about your balances, bills and agents, or have it prepare a payment. Taly prepares; it never pays.
- **Approvals**: when a payment is waiting for you, a notification says so. The app never approves anything
  itself. Open the approval on your phone or on the web, and approve it there with your passkey.
- **Hold to talk**: hold the microphone button, speak, and let go. What you said lands in the message box,
  unsent, so you can check it first.
- **Sign in** with your Talyvor account in your browser. The app keeps its sign-in in the macOS Keychain or the
  Windows Credential Manager. To sign it out everywhere, remove "Talyvor for desktop" under Connected apps on the web.

## Install

Download the latest build from this repository's **Actions** tab: open the newest green `build` run on `main`,
then download **Talyvor-macOS** (a `.dmg`) or **Talyvor-Windows** (a setup `.exe`).

These builds are not signed yet, so the first time you open one:

- **macOS**: open the `.dmg` and drag Talyvor to Applications. Then, in Finder, Control-click Talyvor, choose
  **Open**, and choose **Open** again. After that it opens normally. It lives in the menu bar, not the Dock.
- **Windows**: run the setup `.exe`. If SmartScreen says it protected your PC, choose **More info**, then
  **Run anyway**. Talyvor appears in the tray; drag it out of the overflow area to keep it in view.

## Build it yourself

You need Rust (stable) and Node 24.

```sh
npm ci
npx tauri dev                 # run it against app.talyvor.com
TALYVOR_URL=http://127.0.0.1:8080 npx tauri dev   # or against a local Talyvor
npx tauri build               # an installable build for this computer
cd src-tauri && cargo test    # the tests: sign-in, chat and approvals against a local stand-in
```

## How it reaches Talyvor

Through Talyvor Connect only: the app registers itself, signs in with PKCE through the browser and a loopback
address, and calls the tools at `https://app.talyvor.com/mcp` (`ask_taly`, `list_approvals`, `transcribe`). The
window's page holds no token: everything goes through the app's own commands in `src-tauri/src/lib.rs`.
