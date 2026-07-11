# ryuzi (runner)

Headless engine daemon for [Ryuzi Cockpit](https://github.com/alfin-efendy/ryuzi).
Install it on any machine you want to run agent sessions on; drive it from
the Cockpit desktop app.

## Quick start

    ryuzi setup              # first-run wizard: seed required settings
    ryuzi start              # run the daemon in the foreground
    ryuzi service install    # or: install as a systemd/launchd user service
    ryuzi status             # daemon state (pid, port, version)
    ryuzi doctor             # environment check
    ryuzi config get <key>   # headless settings access

The daemon serves the control API on 127.0.0.1:4483 (setting: `control_port`)
with a bearer token at the state dir's `control.token`. Remote access (TLS +
pairing) ships in a later phase.
