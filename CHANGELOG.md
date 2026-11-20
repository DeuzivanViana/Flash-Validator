# Changelog

All notable changes to FlashValidator are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## 1.0.0 - 2026-11-19

First public release.

### Added

- Three test modes:
  - **Read-only**: never writes. The drive is opened without write access.
    Detects read errors, unstable reads and address wrap-around in existing
    data.
  - **Quick**: 576 regions spread over the declared capacity, each saved,
    written with a random pattern, verified and restored. Includes a
    pre-flight check and a calibration warm-up.
  - **Thorough**: all regions plus power-of-two alias probes are written
    first, then verified, then restored. Detects wrap-around fakes and
    reports the wrap period.
- Crash-recovery journal: original data is saved on the computer before any
  write. `flashvalidator restore` replays it after an interrupted run.
- Reports: declared, validated/readable size, highest good region, a
  576-cell region map, and read/write latency statistics. Text or JSON.
- Desktop app (`flashvalidator-gui`): black theme, live region map,
  confirmation dialog, copy and save report.
- Command-line tool (`flashvalidator`): `list`, `test`, `restore`, JSON
  output, and exit codes that reflect the verdict.
- Elevation on demand: polkit (`pkexec`) or `sudo` on Linux, UAC on Windows.
  The GUI stays unprivileged and talks to a token-authenticated elevated
  helper.
- Linux (sysfs, `O_DIRECT`) and Windows (`\\.\PhysicalDriveN`, volume
  lock/dismount, disk offline) backends.
- Application icon, Windows version information, Linux `.desktop` entry and
  install script.
