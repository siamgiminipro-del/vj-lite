# VJ-Lite — Multi-OJ Contest Platform (Rust + Tauri)

A lightweight Virtual Judge & ICPC Contest Platform built in **Rust** (`Axum` + `SQLite` + `Tauri v2`), designed for up to **50 users** with a 60-30-10 Carbon & Electric Indigo UI.

## Features
- **Google Sign-Up / Sign-In + Contest Access Codes** (`CLUB-2026-W1`).
- **Multi-OJ Local Bridge:** Supports **Codeforces**, **AtCoder**, **CSES**, **CodeChef**, and **Local Judge** problems. User OJ credentials stay locally in the OS Keychain / Tauri WebView and are never sent to the server.
- **Official API Verdict Sync:** Syncs Codeforces (`user.status`) and AtCoder (`atcoder-api/v3/user/submissions`) within the contest window.
- **Built-in Local Judge:** Runs C++17 (including `<bits/stdc++.h>` on macOS), Python 3, and JavaScript.
- **Past 10 Contests Archive & Live ICPC Standings:** Tracks solved count, +20m penalty, first-to-solve, and scoreboard freeze.

## Run Backend Server

```bash
cargo run -p vj-server
```

Open `http://localhost:3000`.
- **Coordinator:** `admin@vjclub.local` / `admin123`
- **Participants:** `alice@gmail.com` / `alice123`, `bob@gmail.com` / `bob123`
- **Access Code:** `CLUB-2026-W1`

## Run Tests

```bash
cargo test -p vj-server
```

## Desktop App (`src-tauri/`)

Install Tauri CLI (`cargo install tauri-cli`), then run:

```bash
cd src-tauri && cargo tauri dev
```
