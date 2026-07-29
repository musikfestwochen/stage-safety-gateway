# Configuration Reference

`--config <path>` is global and may precede any command. Without it, config
lives in the platform config directory. On Linux this is
`$XDG_CONFIG_HOME/stage-safety-gateway/config.toml`, or
`~/.config/stage-safety-gateway/config.toml` when `XDG_CONFIG_HOME` is unset.

`stage-safety-gateway config` opens the interactive editor. Saving validates
the whole file and replaces it atomically. `config validate` is noninteractive
and prints a token-redacted summary.

Wizard supplies initial values. Hand-written TOML has no implicit defaults
except where noted below, rejects unknown fields, and requires at least one
sensor.

```toml
[serial]
port = "/dev/ttyUSB0"
baud_rate = 115200

[aggregation]
change_percent = 20.0
min_change_mps = 1.0
min_interval_secs = 30
max_interval_secs = 300

[[sensor]]
type = "bw-wss"
name = "WINDMESSER01"
id = "FD25D5"
unit = "m/s"
data_tag = "25DF"
gust = true
average_window_secs = 10
gust_window_secs = 10
url = "https://musikfestapp.ch/api/stage-safety/readings"
token = "replace-with-api-token"
```

| Key | Required/default | Meaning |
| --- | --- | --- |
| `serial.port` | Required; wizard `/dev/ttyUSB0` | Nonempty serial device path or name. |
| `serial.baud_rate` | Required; wizard `115200` | Positive serial baud rate. |
| `aggregation.change_percent` | Required; wizard `20.0` | Relative change from last policy-sent value that triggers a send; finite and at least zero. |
| `aggregation.min_change_mps` | Optional; `1.0` | Absolute change floor in metres per second. Effective threshold is the larger of this value and the relative threshold calculated from `change_percent`; finite and at least zero. |
| `aggregation.min_interval_secs` | Required; wizard `30` | Minimum interval for value-change sends; zero allowed. Battery changes bypass it. |
| `aggregation.max_interval_secs` | Required; wizard `300` | Send on first reading received after this heartbeat interval; positive and not below minimum. |
| `sensor.type` | Required; `bw-wss` | Sensor type. `bw-wss` is currently the only supported value. |
| `sensor.name` | Optional; `""` | Human label for prompts and logs. Falls back to `id`; need not be unique. |
| `sensor.id` | Required | Six hexadecimal hardware-ID characters. Sent uppercase as API `sensor_identifier`. |
| `sensor.unit` | Required; wizard `m/s` | Sensor source unit: `m/s`, `km/h`, `mph`, `fps`, or `kn`. HTTP values are converted to `m/s`. |
| `sensor.data_tag` | Required; wizard `"0000"` | Quoted hexadecimal average tag, serialized as four uppercase digits. |
| `sensor.gust` | Optional; `false` | Enables gust frames on `data_tag + 1`. |
| `sensor.average_window_secs` | Required; wizard `10` | Positive source average window. Aggregated sends report elapsed gateway window. |
| `sensor.gust_window_secs` | Optional; `0`; wizard `10` when enabled | Positive and required when `gust = true`; otherwise unused. |
| `sensor.url` | Required; wizard Musikfestapp endpoint for first sensor | Per-sensor HTTP(S) ingestion endpoint. Later additions reuse previous sensor URL. |
| `sensor.token` | Required | Nonempty per-sensor Bearer token. Existing wizard value is retained unless replaced. |

Base and enabled gust tags must not collide across sensors. Gust cannot be
enabled for base tag `FFFF`, because no `data_tag + 1` exists.

`run` loads config once. The wizard remains available while it runs and reports
that a restart is required after saving. With the supported systemd user service:

```sh
systemctl --user restart stage-safety-gateway
```
