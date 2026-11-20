# FlashValidator architecture

FlashValidator is a Cargo workspace with three crates. All the logic lives in
the library. The two front ends only display progress and results, and take
care of asking for elevated rights.

```
 +-----------------------+         +----------------------------------------+
 |  flashvalidator-cli   |         |          flashvalidator-gui            |
 |  clap, ctrlc          |         |  window (user rights)                  |
 |  re-exec via sudo /   |         |    | Worker: in-process thread, or     |
 |  UAC when needed      |         |    | elevated helper over loopback TCP |
 +-----------+-----------+         |  helper mode (root / admin) -----+    |
             |                     +----------------------------------|----+
             |        Event stream (observer)                         |
             +--------------------------+-----------------------------+
                                        v
+--------------------------------------------------------------------------+
| flashvalidator-core                                                      |
|  device     list_devices / find_device / DeviceInfo::open(Access)        |
|    +-- platform::linux    sysfs; O_DIRECT (+O_SYNC|O_EXCL for writes)    |
|    +-- platform::windows  IOCTLs; lock/dismount + offline for writes     |
|  privilege  is_elevated / can_open_raw / spawn_elevated (pkexec,sudo,UAC)|
|  blockdev   BlockDevice trait, Access, error classification              |
|  engine     Validator: read-only / quick / thorough                      |
|  layout, rng, journal, stats, report (Serialize + Deserialize)           |
|  sim        (tests only) simulated genuine and counterfeit drives        |
+--------------------------------------------------------------------------+
```

## Key abstractions

### `BlockDevice` and `Access`

```rust
pub trait BlockDevice: Send {
    fn size_bytes(&self) -> u64;
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    fn identity(&self) -> String;
}
```

The engine always uses 4 KiB-aligned offsets, lengths and buffers
(`aligned::AlignedBuffer`), as unbuffered I/O on both operating systems
requires.

`DeviceInfo::open(Access::ReadOnly | Access::ReadWrite)`:

| | ReadOnly | ReadWrite |
|---|---|---|
| Linux | `O_RDONLY \| O_DIRECT`. Works while mounted. | `O_RDWR \| O_DIRECT \| O_SYNC \| O_EXCL`. `EBUSY` if mounted. |
| Windows | `GENERIC_READ`, `FILE_FLAG_NO_BUFFERING`. Volumes untouched. | Read/write, no-buffering + write-through, `FSCTL_ALLOW_EXTENDED_DASD_IO`, volumes locked and dismounted, disk set offline with `IOCTL_DISK_SET_DISK_ATTRIBUTES` (not persistent). `Drop` brings it back online. |

With `ReadOnly`, the OS handle has no write permission. A bug could not modify
the drive even if it tried.

`blockdev::classify` sorts OS errors into three kinds:

- `DeviceGone`: stop the run and keep the journal.
- `WriteProtected`: reported by the pre-flight check.
- `Other`: counted against the region being tested.

### `Validator` and events

`Validator::run(observer)` is synchronous and emits `Event`s (`Started`,
`Phase`, `Activity`, `Calibration`, `Region`, `Progress`, `Warning`). All
events and the final `Report` implement `Serialize` + `Deserialize`, so they
can be sent across the privilege boundary as JSON lines.

Cancellation uses a shared `CancelToken`. The engine stops only at safe points
and always restores data it has written.

Setup failures (`ReadFailure`, `WriteProtected`, `TooSmall`, journal creation)
return `Err`. Once testing has started the result is always a `Report`, with
`completed == false` and `Verdict::Incomplete` for interrupted runs.

## Algorithms

### Region layout and order (`layout`)

There are 576 regions by default.
`offset(r) = round(r * span / 575) * 512`, aligned down to 4 KiB, where
`span = sectors - block/512`. This is computed exactly in `u128`. Region 0
starts at byte 0 and the last region touches the declared end.

The order is the last region first (the end of the claimed capacity, where
fakes fail), then the first, then the rest uniformly at random. Random
patterns come from xorshift128 seeded with OS entropy for every run, so a
controller cannot predict them.

### Quick mode

1. **Pre-flight**: read the first 4 KiB (trying up to 32 consecutive blocks),
   then write the same bytes back. This catches dead and write-protected drives
   before anything changes.
2. **Calibration**: the region test runs on the last block of the drive with
   transfers from 4 KiB to 8 MiB. This is a warm-up that exercises the end of
   the drive, and it is not counted in the statistics.
3. **Regions**: for each region, read the original, journal it, write a random
   pattern (always different from the original), read back, compare, restore.
   If the restore fails, OK or read-error becomes write-error.

### Thorough mode

Probes are the 576 regions plus {2^k, 3·2^k : k ≥ 20}. All probes are backed
up, all are written with tagged patterns (magic, run nonce, probe index,
offset, then pseudo-random data), then flushed, verified and restored.

A probe that reads back another probe's tag is grouped with it (union-find).
In each group:

- the lowest offset is real storage;
- the GCD of the distances is the wrap period;
- everything at or above the period is marked "no storage".

### Read-only mode

1. Pre-flight read of the first sectors. No calibration and no journal, since
   nothing is written.
2. Two read passes over all probes, each in its own random order. If the two
   reads of a block differ, the block is "no storage" (unstable reads).
3. **Alias analysis on existing data:** non-blank blocks are grouped by
   content hash. A period is accepted only with corroboration (two groups that
   agree on it, or one group with three or more members). Otherwise identical
   blocks are only reported as a note, because they occur on genuine drives.
4. **Blank analysis:** a note reports where the drive stops holding data. A
   read-only check cannot tell an empty area from missing flash.
5. The verdict is `ReadOnlyClean` ("no problems found, capacity not proven"),
   `Counterfeit`, `Errors` or `Incomplete`.

### Journal (`*.fvj`)

```text
header : "FLVJRN01" | u16 identity_len | identity | u64 device_size
backup : 0x01 | u64 offset | u32 len | data | u64 fnv1a
restore: 0x02 | u64 offset | u32 len | u64 fnv1a
```

Before any pattern write, the original block is appended and `fdatasync`ed.
After a successful restore, a marker is appended. A torn trailing record is
ignored. Replay applies pending records newest first, so the oldest (true)
original wins where records overlap. Journals are bound to the drive's
identity (serial + size, or path + size).

## Privilege model

The desktop app never runs as root or administrator. When a test starts:

1. `privilege::can_open_raw(path, write)`: is the process elevated, or (on
   Linux) does `access(2)` allow it, e.g. through the `disk` group? If yes, the
   run happens on a background thread in the same process.
2. Otherwise the app:
   - binds `127.0.0.1:0` and creates a 128-bit random token;
   - starts *its own executable* through `privilege::spawn_elevated`
     (`pkexec` on Linux, `ShellExecuteExW("runas")` on Windows) as
     `flashvalidator-gui --helper --connect 127.0.0.1:PORT --token T --device PATH --mode M`;
   - waits for the connection. If the elevation process exits first, the
     prompt was declined (pkexec exit code 126/127, or `ERROR_CANCELLED` from
     UAC). The helper connects *before* opening the drive, so stopping it at
     this stage is always safe.
3. **Helper protocol** (one JSON object per line):
   - helper → app: `{"hello":{"token":T}}` first. The app drops the helper if
     the token is wrong.
   - helper → app: `{"event":...}` repeatedly, then `{"done":{"Ok":report}}`
     or `{"done":{"Err":msg}}`.
   - app → helper: `cancel`. If the socket closes (window closed), the helper
     cancels too. Either way it restores data and then exits.
   - The helper accepts only `127.0.0.1` addresses and fixed arguments. It
     opens only the drive named on its command line, after looking it up again
     with elevated rights.

The command line uses the same module. It confirms in the current terminal,
then re-executes itself with `--yes` through `sudo` (Linux), or through UAC in
a new console window that stays open at the end (Windows). It returns the
elevated process's exit code.

## Desktop app

- `theme.rs`: pure-black background, near-black surfaces, a teal accent and a
  status palette (green validated, sky readable, red no storage, amber read
  error, violet write error). Corners are 1–2 px, there are no shadows.
- `worker.rs`: chooses in-process or elevated execution and forwards
  `Message::{Status, Event, Done}` to the UI over an `mpsc` channel.
  Dropping the worker cancels and joins, so closing the window mid-test still
  restores data.
- `helper.rs`: helper-mode entry point and protocol types.
- `map.rs`: 32 × 18 region map with tooltips, and a legend that adapts to the
  mode.
- `assets/icon_art.rs`: the icon, drawn procedurally. It is used for the
  window icon and, through the build scripts, for the Windows `.ico`.
  `assets/flashvalidator.svg` draws the same geometry for Linux.

## Testing strategy

- Unit tests per module: layout, rng, stats, journal, report (including a JSON
  round trip), privilege (Windows argument quoting), helper arguments.
- Engine scenario tests on simulated drives:
  - Quick and Thorough: genuine, discarding, wrapping (Quick misses it,
    Thorough catches it), read errors, cancellation, journal cleanup. Each
    checks with a content fingerprint that the data is unchanged.
  - Read-only: genuine, wrapping, unstable, discarding and failing drives.
    Each asserts that **zero writes** were issued.
- Integration test (`crates/flashvalidator-gui/tests/helper_protocol.rs`): runs
  the real helper binary without elevation and checks the token handshake and
  the result message.
- CI (`.github/workflows/ci.yml`) runs fmt, clippy (`-D warnings`), tests and
  release builds on Ubuntu and Windows. `release.yml` packages and publishes
  tagged releases.

## Possible future work

- Detect drive hot-plug (udev / `RegisterDeviceNotification`).
- A polkit action file for a friendlier Linux authentication message.
- macOS backend (`/dev/rdiskN`, `F_NOCACHE`, DiskArbitration unmount).
- Restore journals from the desktop app.
