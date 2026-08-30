# Project VISOR

## Overview

**Project VISOR** is a lightweight Windows background application written primarily in **Rust**.

Its initial purpose is to use a webcam to determine whether the user is physically present at their computer. Based on the detected presence state, VISOR can control peripherals and applications to reduce unnecessary power usage and eventually provide contextual automation.

The first target use case is:

> Detect whether the user is sitting in front of the computer → if absent for a configurable amount of time, turn off or put the OLED monitor into a power-saving state → when the user returns, restore the monitor.

The project should be designed from the beginning so additional presence-based features can be added later without rewriting the core application.

---

# Core Philosophy

VISOR should be:

- Lightweight
- Low CPU usage
- Low memory usage
- Fast to start
- Able to run continuously in the background
- Privacy-conscious
- Modular
- Reliable
- Windows-first
- Written primarily in Rust
- Configurable without requiring code changes

The application should avoid unnecessary dependencies and background processing.

The webcam should be processed locally.

**No webcam images or video should ever be uploaded to an external service.**

---

# Why Rust?

Rust is preferred because VISOR is intended to run continuously in the background.

Important goals:

- Minimal memory footprint
- Low CPU usage when idle
- No unnecessary runtime overhead
- Strong reliability
- Good Windows integration
- Easy distribution as a standalone executable
- Safe concurrency
- Easy access to native Windows APIs when required

Avoid introducing large frameworks unless they provide a clear benefit.

---

# Initial Architecture

The application should be designed around independent components.

Suggested architecture:

use claude design to design the UI if needed

```text
VISOR
│
├── Core
│   ├── Presence State
│   ├── State Machine
│   ├── Timers
│   └── Configuration
│
├── Vision
│   ├── Webcam Capture
│   ├── Face Detection
│   └── Detection Confidence
│
├── Actions
│   ├── OLED Control
│   └── Future Actions
│
├── Integrations
│   └── Discord Presence
│
├── UI
│   ├── System Tray
│   ├── Status
│   └── Settings
│
└── Logging
    ├── Errors
    └── Diagnostics