use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use crossterm::cursor::MoveTo;
use crossterm::style::Stylize;
use crossterm::terminal::{Clear, ClearType};
use inquire::ui::{Color, RenderConfig, StyleSheet, Styled};
use inquire::validator::Validation;
use inquire::{Confirm, Password, PasswordDisplayMode, Select, Text};
use stage_safety_gateway::config::{
    AggregationConfig, BwWssSensor, Config, Sensor, SerialConfig, WindUnit, DEFAULT_URL,
};
use stage_safety_gateway::{run_reader, ReaderEvent};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(
    version,
    about = "Stage-safety sensor gateway for Musikfestapp (Broadweigh/Mantracourt)"
)]
struct Cli {
    /// Config file path.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create or edit the configuration interactively.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Start the gateway daemon (not implemented yet).
    Run,
    /// Decode serial input (or stdin) and print parsed readings.
    /// Diagnostic mode: no network, no daemon. Resync noise and dropped
    /// unconfigured frames surface as `[…]` lines between readings.
    Listen {
        /// Read raw bytes from stdin instead of `serial.port`. Replay a capture
        /// with e.g. `xxd -r -p tests/fixtures/bw-wss-mps.hex | stage-safety-gateway listen --stdin`.
        #[arg(long)]
        stdin: bool,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Validate the configuration and print a redacted summary.
    Validate,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = match cli.config {
        Some(path) => path,
        None => default_path()?,
    };
    match cli.command {
        Commands::Config { action: None } => wizard(&path),
        Commands::Config {
            action: Some(ConfigAction::Validate),
        } => {
            let config = Config::load(&path)?;
            println!("{}\n\nconfig OK: {}", config.summary(), path.display());
            Ok(())
        }
        Commands::Run => bail!("`run` is not implemented yet"),
        Commands::Listen { stdin } => {
            let config = Config::load(&path)?;
            listen(&config, stdin)
        }
    }
}

fn default_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("cannot determine platform config dir; pass --config")?;
    Ok(dir.join("stage-safety-gateway").join("config.toml"))
}

fn wizard(path: &Path) -> Result<()> {
    inquire::set_global_render_config(theme());
    let mut config = if path.exists() {
        match Config::read(path) {
            Ok(config) => config,
            Err(e) => {
                warn(&format!("cannot load existing config:\n{e:#}"));
                if Confirm::new("Start from defaults instead? (overwrites the file on save)")
                    .with_default(false)
                    .prompt()?
                {
                    default_config()
                } else {
                    return Err(e);
                }
            }
        }
    } else {
        default_config()
    };

    loop {
        crossterm::execute!(std::io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
        let action = Select::new(
            &format!("Configuration ({})", path.display()),
            vec![
                "Serial settings",
                "Aggregation policy",
                "Add sensor",
                "Edit sensor",
                "Remove sensor",
                "List sensors",
                "Save and exit",
            ],
        )
        .prompt()?;
        match action {
            "Serial settings" => edit_serial(&mut config)?,
            "Aggregation policy" => edit_aggregation(&mut config)?,
            "Add sensor" => add_sensor(&mut config)?,
            "Edit sensor" => edit_sensor(&mut config)?,
            "Remove sensor" => remove_sensor(&mut config)?,
            "List sensors" => list_sensors(&config)?,
            _ => match config.save(path) {
                Ok(()) => {
                    note(&format!("saved to {}", path.display()));
                    return Ok(());
                }
                Err(e) => {
                    println!("\n{e:#}\n\nfix the problems above, then save again");
                    pause()?;
                }
            },
        }
    }
}

fn default_config() -> Config {
    Config {
        serial: SerialConfig {
            port: "/dev/ttyUSB0".into(),
            baud_rate: 115200,
        },
        aggregation: AggregationConfig {
            change_percent: 20.0,
            min_interval_secs: 30,
            max_interval_secs: 300,
        },
        sensors: Vec::new(),
    }
}

fn theme() -> RenderConfig<'static> {
    let mut config = RenderConfig::default_colored();
    config.prompt_prefix = Styled::new("❯").with_fg(Color::LightCyan);
    config.highlighted_option_prefix = Styled::new("→").with_fg(Color::LightCyan);
    config.selected_option = Some(StyleSheet::new().with_fg(Color::LightCyan));
    config.help_message = StyleSheet::new().with_fg(Color::DarkGrey);
    config
}

/// Padded, colored status line that stays visible between prompts.
fn note(message: &str) {
    println!("\n  {} {message}\n", "✓".green());
}

fn warn(message: &str) {
    println!("\n  {} {message}\n", "!".yellow());
}

/// Blocks output until acknowledged, so the menu redraw can't erase it unread.
fn pause() -> Result<()> {
    println!("  {}", "press any key to continue".dark_grey());
    crossterm::terminal::enable_raw_mode()?;
    let read = crossterm::event::read();
    crossterm::terminal::disable_raw_mode()?;
    read?;
    Ok(())
}

fn edit_serial(config: &mut Config) -> Result<()> {
    const MANUAL: &str = "Other (enter manually)";
    let ports = serialport::available_ports().unwrap_or_default();
    if ports.is_empty() {
        warn("no serial ports detected; enter the path manually");
        config.serial.port = Text::new("Serial port:")
            .with_default(&config.serial.port)
            .prompt()?;
    } else {
        let mut options: Vec<String> = ports.iter().map(port_label).collect();
        options.push(MANUAL.into());
        let cursor = ports
            .iter()
            .position(|p| p.port_name == config.serial.port)
            .unwrap_or(options.len() - 1);
        let pick = Select::new("Serial port:", options)
            .with_starting_cursor(cursor)
            .prompt()?;
        config.serial.port = if pick == MANUAL {
            Text::new("Serial port:")
                .with_default(&config.serial.port)
                .prompt()?
        } else {
            let index = ports
                .iter()
                .position(|p| port_label(p) == pick)
                .unwrap_or(0);
            ports[index].port_name.clone()
        };
    }

    let baud_default = config.serial.baud_rate.to_string();
    let baud = Text::new("Baud rate:")
        .with_default(&baud_default)
        .with_validator(positive_u32)
        .prompt()?;
    config.serial.baud_rate = baud.parse()?;
    Ok(())
}

fn port_label(info: &serialport::SerialPortInfo) -> String {
    let kind = match &info.port_type {
        serialport::SerialPortType::UsbPort(usb) => usb.product.as_deref().unwrap_or("USB device"),
        serialport::SerialPortType::PciPort => "PCI",
        serialport::SerialPortType::BluetoothPort => "Bluetooth",
        _ => "serial",
    };
    format!("{} ({})", info.port_name, kind)
}

fn edit_aggregation(config: &mut Config) -> Result<()> {
    let percent_default = config.aggregation.change_percent.to_string();
    let min_default = config.aggregation.min_interval_secs.to_string();
    let max_default = config.aggregation.max_interval_secs.to_string();
    let percent = Text::new("Send when a value changes by (%):")
        .with_default(&percent_default)
        .with_help_message(
            "immediate send when a reading differs from the last sent value by this much; \
             applies to average and gust independently; 20 is a good start",
        )
        .with_validator(|input: &str| {
            Ok(match input.parse::<f64>() {
                Ok(value) if value.is_finite() && value >= 0.0 => Validation::Valid,
                _ => Validation::Invalid("enter a finite number >= 0".into()),
            })
        })
        .prompt()?;
    let min = Text::new("Minimum send interval (seconds):")
        .with_default(&min_default)
        .with_help_message("rate limit for value changes; battery changes still send immediately")
        .with_validator(min_value(0))
        .prompt()?;
    let max = Text::new("Maximum send interval (seconds):")
        .with_default(&max_default)
        .with_help_message("always send at least this often, even when readings are stable")
        .with_validator(min_value(1))
        .prompt()?;
    config.aggregation.change_percent = percent.parse()?;
    config.aggregation.min_interval_secs = min.parse()?;
    config.aggregation.max_interval_secs = max.parse()?;
    if config.aggregation.min_interval_secs > config.aggregation.max_interval_secs {
        warn("minimum interval exceeds maximum; config will not validate");
        pause()?;
    }
    Ok(())
}

fn add_sensor(config: &mut Config) -> Result<()> {
    println!("sensor type: bw-wss (only supported type for now)");

    let def_url = match config.sensors.last() {
        Some(Sensor::BwWss(last)) => last.url.clone(),
        None => DEFAULT_URL.to_string(),
    };
    let blank = BwWssSensor {
        name: String::new(),
        id: String::new(),
        unit: WindUnit::Mps,
        data_tag: 0,
        gust: false,
        average_window_secs: 10,
        gust_window_secs: 0,
        url: def_url,
        token: String::new(),
    };
    let sensor = prompt_sensor_fields(&blank, config, None)?;
    config.sensors.push(Sensor::BwWss(sensor));
    note("sensor added");
    pause()
}

fn edit_sensor(config: &mut Config) -> Result<()> {
    if config.sensors.is_empty() {
        warn("no sensors configured");
        return pause();
    }
    let pick = Select::new(
        "Edit which sensor?",
        config.sensors.iter().map(Sensor::details).collect(),
    )
    .prompt()?;
    let index = config
        .sensors
        .iter()
        .position(|s| s.details() == pick)
        .context("selected sensor vanished")?;
    let Sensor::BwWss(existing) = &config.sensors[index];
    let existing = existing.clone();
    let updated = prompt_sensor_fields(&existing, config, Some(index))?;
    config.sensors[index] = Sensor::BwWss(updated);
    note("sensor updated");
    pause()
}

/// Prompts every `BwWssSensor` field, prefilled from `default`. On edit
/// (`skip` = `Some(i)`) the token is reused unless the user explicitly asks to
/// replace it; the token is never printed. On add (`skip` = `None`) a fresh
/// token is required. The data-tag collision check ignores index `skip` so a
/// sensor being edited doesn't collide with itself.
fn prompt_sensor_fields(
    default: &BwWssSensor,
    config: &Config,
    skip: Option<usize>,
) -> Result<BwWssSensor> {
    let name = Text::new("Sensor name:")
        .with_default(&default.name)
        .prompt()?;
    let id = Text::new("Hardware ID (6 hex chars):")
        .with_default(&default.id)
        .with_validator(|input: &str| {
            Ok(
                if input.len() == 6 && input.chars().all(|c| c.is_ascii_hexdigit()) {
                    Validation::Valid
                } else {
                    Validation::Invalid("6 hex characters, e.g. 1A2B3F".into())
                },
            )
        })
        .prompt()?;
    let data_tag = loop {
        let tag_default = format!("{:04X}", default.data_tag);
        let input = Text::new("Base data tag (hex):")
            .with_default(&tag_default)
            .with_validator(|input: &str| {
                Ok(if u16::from_str_radix(input, 16).is_ok() {
                    Validation::Valid
                } else {
                    Validation::Invalid("hex value between 0000 and FFFF".into())
                })
            })
            .prompt()?;
        let tag = u16::from_str_radix(&input, 16)?;
        if let Some(owner) = config.tag_owner(tag, skip) {
            println!("tag {tag:04X} is already used by {owner}");
        } else {
            break tag;
        }
    };
    let unit = Select::new("Unit:", WindUnit::ALL.to_vec())
        .with_starting_cursor(
            WindUnit::ALL
                .iter()
                .position(|u| *u == default.unit)
                .unwrap_or(0),
        )
        .prompt()?;
    let avg_default = default.average_window_secs.to_string();
    let average_window_secs: u64 = Text::new("Average window (seconds):")
        .with_default(&avg_default)
        .with_validator(min_value(1))
        .prompt()?
        .parse()?;
    let gust = Confirm::new("Gust measurement enabled?")
        .with_default(default.gust)
        .prompt()?;
    let gust_window_secs: u64 = if gust {
        let gust_default = if default.gust_window_secs == 0 {
            "10".to_string()
        } else {
            default.gust_window_secs.to_string()
        };
        Text::new("Gust window (seconds):")
            .with_default(&gust_default)
            .with_validator(min_value(1))
            .prompt()?
            .parse()?
    } else {
        0
    };
    let url = Text::new("Musikfestapp URL:")
        .with_default(&default.url)
        .with_validator(|input: &str| {
            Ok(
                if input.starts_with("http://") || input.starts_with("https://") {
                    Validation::Valid
                } else {
                    Validation::Invalid("must start with http(s)://".into())
                },
            )
        })
        .prompt()?;
    let token = if skip.is_some()
        && !Confirm::new("Replace API token?")
            .with_default(default.token.is_empty())
            .prompt()?
    {
        default.token.clone()
    } else {
        Password::new("API token:")
            .with_display_mode(PasswordDisplayMode::Masked)
            .without_confirmation()
            .with_validator(|input: &str| {
                Ok(if input.is_empty() {
                    Validation::Invalid("token must not be empty".into())
                } else {
                    Validation::Valid
                })
            })
            .prompt()?
    };

    Ok(BwWssSensor {
        name,
        id,
        unit,
        data_tag,
        gust,
        average_window_secs,
        gust_window_secs,
        url,
        token,
    })
}

fn list_sensors(config: &Config) -> Result<()> {
    if config.sensors.is_empty() {
        warn("no sensors configured");
        return pause();
    }
    println!();
    for sensor in &config.sensors {
        println!("  - {}", sensor.details());
    }
    println!();
    pause()
}

fn remove_sensor(config: &mut Config) -> Result<()> {
    if config.sensors.is_empty() {
        warn("no sensors configured");
        return pause();
    }
    let pick = Select::new(
        "Remove which sensor?",
        config.sensors.iter().map(Sensor::details).collect(),
    )
    .prompt()?;
    if let Some(index) = config.sensors.iter().position(|s| s.details() == pick) {
        config.sensors.remove(index);
        note(&format!("removed {pick}"));
    }
    pause()
}

fn min_value(min: u64) -> impl Fn(&str) -> Result<Validation, inquire::CustomUserError> + Clone {
    move |input: &str| {
        Ok(match input.parse::<u64>() {
            Ok(value) if value >= min => Validation::Valid,
            _ => Validation::Invalid(format!("enter an integer >= {min}").into()),
        })
    }
}

fn positive_u32(input: &str) -> Result<Validation, inquire::CustomUserError> {
    Ok(match input.parse::<u32>() {
        Ok(value) if value > 0 => Validation::Valid,
        _ => Validation::Invalid("enter a positive integer".into()),
    })
}

fn listen(config: &Config, stdin: bool) -> Result<()> {
    if stdin {
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        return Ok(listen_once(&mut lock, config)?);
    }

    loop {
        match serialport::new(&config.serial.port, config.serial.baud_rate)
            .timeout(Duration::from_secs(1))
            .open()
        {
            Ok(mut port) => match listen_once(&mut port, config) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Ok(()),
                Err(e) => {
                    eprintln!("  ! serial read error: {e}");
                    std::thread::sleep(Duration::from_secs(1));
                }
            },
            Err(e) => {
                eprintln!("  ! cannot open {}: {e}", config.serial.port);
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn listen_once<R: Read>(input: &mut R, config: &Config) -> std::io::Result<()> {
    let mut frames = 0u64;
    let mut drained = 0usize;
    let mut dropped = 0u64;
    run_reader(input, config, |event| match event {
        ReaderEvent::Reading(r) => {
            frames += 1;
            println!("{r}");
        }
        ReaderEvent::Drained(n) => {
            drained += n;
            eprintln!("[+{n} bytes ignored resync]  total discarded: {drained}");
        }
        ReaderEvent::Unconfigured(frame) => {
            dropped += 1;
            eprintln!(
                "[dropped: valid frame tag {:04X} value {:.4} rssi={}dBm cv={}]  total dropped: {dropped}",
                frame.tag, frame.value, frame.rssi_dbm, frame.cv
            );
        }
        ReaderEvent::Eof => {
            eprintln!("[end of input]  frames={frames} discarded={drained} dropped={dropped}");
        }
    })?;
    Ok(())
}
