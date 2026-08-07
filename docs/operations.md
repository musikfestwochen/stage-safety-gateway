# Operations Guide

## Sending And Reliability

Average and gust maintain independent send-policy state. First readings and
battery-state changes send immediately. Value changes respect
`min_interval_secs`; stable values send with first reading received after
`max_interval_secs`. Average changes may trigger in either direction. Gust
changes trigger only when the window maximum rises. For both kinds, effective
change threshold is the larger of the relative threshold calculated from
`change_percent` and `min_change_mps`, after converting readings to metres per
second. Aggregated gusts retain the observation timestamp of their maximum;
their window length and send-policy interval still end at the reading that
triggered the send. Battery, RSSI, and CV metadata also come from that latest
reading.

Each sensor has an independent FIFO queue containing up to 100 pending requests,
plus a possible in-flight request. Network failures, timeouts, HTTP 408, HTTP
429, and HTTP 5xx retry the unchanged in-flight request before later requests.
Backoff starts at one second and doubles to 60 seconds; an integer-seconds
`Retry-After` on HTTP 429 overrides that attempt's delay, capped at 60 seconds.
HTTP requests time out after 30 seconds. Permanent HTTP failures are logged and
discarded.

Queues exist only in memory. A full queue drops its oldest pending request, not
the in-flight request. Shutdown reports but does not drain pending requests;
process failure or restart also loses them. There is no disk spool or guaranteed
delivery. Invalid frames, unconfigured tags, and readings missed during serial
disconnects cannot be recovered.

## Logging

`run` stays in the foreground and handles SIGINT/SIGTERM for service-manager
use. Normal mode logs gateway events at `info`; `--verbose` enables `debug`
measurements, policy decisions, normalized payloads, and HTTP outcomes. Set
`RUST_LOG` to override either default, for example:

```sh
RUST_LOG=stage_safety_gateway=debug,reqwest=warn stage-safety-gateway run
```

Normal mode also reports first readings, battery and connection transitions,
detailed retry decisions, and five-minute sensor/serial health counters. Final
health counters and uptime are logged during clean shutdown.

Configured API tokens and URL credentials/query parameters are excluded from
gateway logs. Verbose logs still contain sensor IDs, measurements, timestamps,
and radio metadata; restrict access to captured logs.

## Service Operation

`run` does not daemonize or prompt. Initial config, invalid endpoint, or
serial-open failure exits nonzero; an unavailable API retries at runtime and
serial disconnects reconnect every second. On Unix the serial port opens
exclusively. `run` and serial `listen` also hold a per-user runtime lock, so a
second command exits immediately instead of competing for input. Config editing,
validation, and `listen --stdin` remain available while the gateway runs.

Use the same Linux user for installation, configuration, and the service. This
keeps the default config path and its `0600` permissions working without a
dedicated account or files under `/etc`.

Enable systemd user services at boot without requiring a login:

```sh
sudo loginctl enable-linger "$USER"
```

`enable-linger` starts the user's service manager during boot and keeps it
running after logout. Serial access often works already, including on typical
Raspberry Pi installations. If logs report permission denied for the configured
device, add the current user to that device's group, commonly:

```sh
sudo usermod -aG dialout "$USER"
```

The group may differ from `dialout`. Reboot after changing membership so the
lingering user manager receives the new groups.

The binary embeds `packaging/systemd/stage-safety-gateway.service`:

```ini
[Unit]
Description=Stage Safety Gateway
StartLimitIntervalSec=0

[Service]
Type=simple
ExecStart=%h/.cargo/bin/stage-safety-gateway run
Restart=always
RestartSec=5
TimeoutStopSec=35

[Install]
WantedBy=default.target
```

Install and start it after creating valid config:

```sh
stage-safety-gateway service install
```

The installer writes the unit to `~/.config/systemd/user`, substitutes the
current executable and config paths, reloads systemd, enables boot startup, and
starts or restarts the service. Global `--config <path>` is honored. Running it
again updates an existing installed unit. No network ordering is needed because
HTTP failures retry internally.
`Restart=always` recovers failures and unexpected clean exits; manual
`systemctl --user stop` remains stopped. Disabling systemd's start limit keeps
retrying if config or the serial device is temporarily unavailable.

Check boot setup and follow logs:

```sh
loginctl show-user "$USER" -p Linger
systemctl --user is-enabled stage-safety-gateway
systemctl --user status stage-safety-gateway
journalctl --user -u stage-safety-gateway -f
```

SIGTERM performs a bounded graceful shutdown; an active HTTP attempt may take up
to 30 seconds. `TimeoutStopSec=35` leaves enough time before systemd forces exit.

### Configuration Changes

The wizard remains usable while the service owns the serial port. Port listing
does not open devices. The running process keeps its loaded config until
restarted; after saving, apply changes with:

```sh
stage-safety-gateway config
systemctl --user restart stage-safety-gateway
```

Config replacement is atomic and synced to disk, so the service sees either the
complete old file or the complete new file, including across an abrupt restart.

### Serial Diagnostics

Serial `listen` cannot run beside the service. Stop and restore it explicitly:

```sh
systemctl --user stop stage-safety-gateway
stage-safety-gateway listen
systemctl --user start stage-safety-gateway
```

`listen --stdin` does not access serial hardware and can run without stopping
the service.

## Security

API tokens are stored as plaintext in TOML and sent as Bearer credentials. Use
HTTPS, do not put secrets in endpoint URLs, do not commit real config, and limit
config readability to the service user. Wizard saves create or tighten the file
to mode `0600` on Unix; secure hand-written files yourself:

```sh
chmod 600 /path/to/config.toml
```

## Fixture Replay

From a repository checkout, convert fixture hexadecimal text to raw bytes and
feed it directly to diagnostic mode:

```sh
xxd -r -p tests/fixtures/bw-wss-mps.hex |
  stage-safety-gateway --config /path/to/config.toml listen --stdin
```

Config must map base tag `25DF`, enable gust, and use matching source unit. The
fixture contains 316 valid frames: 158 average and 158 gust. To test `run`
through a virtual serial device instead, create a PTY pair:

```sh
xxd -r -p tests/fixtures/bw-wss-mps.hex > /tmp/bw-wss.raw
socat -d -d pty,raw,echo=0 pty,raw,echo=0
# Configure one printed PTY as serial.port, then in another shell:
dd if=/tmp/bw-wss.raw of=/dev/pts/N status=none
```
