# Operations Guide

## Sending And Reliability

Average and gust maintain independent send-policy state. First readings and
battery-state changes send immediately. Value changes respect
`min_interval_secs`; stable values send with first reading received after
`max_interval_secs`. Average changes may trigger in either direction. Gust
changes trigger only when the window maximum rises by `change_percent`.

Each sensor has an independent FIFO queue containing up to 100 pending requests,
plus a possible in-flight request. Network failures, timeouts, HTTP 408, HTTP
429, and HTTP 5xx retry the unchanged in-flight request before later requests.
Backoff starts at one second and doubles to 60 seconds; an integer-seconds
`Retry-After` on HTTP 429 overrides that attempt's delay. HTTP requests time out
after 30 seconds. Permanent HTTP failures are logged and discarded.

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

Configured API tokens and URL credentials/query parameters are excluded from
gateway logs. Verbose logs still contain sensor IDs, measurements, timestamps,
and radio metadata; restrict access to captured logs.

## Service Operation

`run` does not daemonize or prompt. Initial config, invalid endpoint, or
serial-open failure exits nonzero; an unavailable API retries at runtime and
serial disconnects reconnect every second. On Unix the serial port opens
exclusively, preventing two instances from consuming the same input.

Example systemd unit:

```ini
[Unit]
Description=Stage Safety Gateway
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=stage-safety
SupplementaryGroups=dialout
Environment=RUST_LOG=stage_safety_gateway=info
ExecStart=/usr/local/bin/stage-safety-gateway --config /etc/stage-safety-gateway/config.toml run
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
```

Adjust executable, config path, user, and serial-device group. SIGTERM performs
a bounded graceful shutdown; active HTTP attempt may take up to 30 seconds.

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
