use anyhow::{bail, Context, Result};
use log::{debug, error, info, warn};
use stage_safety_gateway::config::{Config, Sensor};
use stage_safety_gateway::http::{IngestionClient, IngestionRequest, TransportOutcome};
use stage_safety_gateway::{run_reader_until, PolicyState, ReaderEvent, ReadingKind};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 100;
const SERIAL_TIMEOUT: Duration = Duration::from_secs(1);
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
const HEALTH_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Default)]
struct SensorHealth {
    received: u64,
    queued: u64,
    sent: u64,
    suppressed: u64,
    retries: u64,
    permanent_failures: u64,
    dropped: u64,
    last_reading: Option<Instant>,
    rssi_dbm: i16,
    cv: u8,
    battery_low: Option<bool>,
}

#[derive(Default)]
struct SensorStats(Mutex<SensorHealth>);

impl SensorStats {
    fn lock(&self) -> MutexGuard<'_, SensorHealth> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn record_reading(&self, rssi_dbm: i16, cv: u8, battery_low: bool) -> (bool, Option<bool>) {
        let mut health = self.lock();
        let first = health.last_reading.is_none();
        let battery_transition = match health.battery_low {
            Some(previous) if previous != battery_low => Some(battery_low),
            None if battery_low => Some(true),
            _ => None,
        };
        health.received += 1;
        health.last_reading = Some(Instant::now());
        health.rssi_dbm = rssi_dbm;
        health.cv = cv;
        health.battery_low = Some(battery_low);
        (first, battery_transition)
    }

    fn snapshot(&self) -> SensorHealth {
        *self.lock()
    }
}

#[derive(Clone, Copy, Default)]
struct SerialHealth {
    valid_readings: u64,
    discarded_bytes: u64,
    unconfigured_frames: u64,
    reconnects: u64,
}

#[derive(Clone)]
struct Route {
    queue: Arc<RequestQueue>,
    stats: Arc<SensorStats>,
}

#[derive(Default)]
struct QueueState {
    pending: VecDeque<IngestionRequest>,
    closed: bool,
}

#[derive(Default)]
struct RequestQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

impl RequestQueue {
    fn push(&self, request: IngestionRequest) -> Option<IngestionRequest> {
        let mut state = self.lock();
        if state.closed {
            return Some(request);
        }
        let dropped = if state.pending.len() == QUEUE_CAPACITY {
            state.pending.pop_front()
        } else {
            None
        };
        state.pending.push_back(request);
        self.changed.notify_one();
        dropped
    }

    fn pop(&self) -> Option<IngestionRequest> {
        let mut state = self.lock();
        while state.pending.is_empty() && !state.closed {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if state.closed {
            None
        } else {
            state.pending.pop_front()
        }
    }

    fn wait_retry(&self, duration: Duration) -> bool {
        let state = self.lock();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, duration, |state| !state.closed)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !state.closed
    }

    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        self.changed.notify_all();
    }

    fn len(&self) -> usize {
        self.lock().pending.len()
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct SenderWorker {
    name: String,
    id: String,
    queue: Arc<RequestQueue>,
    stats: Arc<SensorStats>,
    handle: JoinHandle<usize>,
}

pub fn run(config: Config, path: &Path) -> Result<()> {
    let config = Arc::new(config);
    let mut senders = Vec::with_capacity(config.sensors.len());
    let mut routes = HashMap::new();
    let serial_health = Arc::new(Mutex::new(SerialHealth::default()));
    let started = Instant::now();

    for configured in &config.sensors {
        let Sensor::BwWss(sensor) = configured;
        let client = IngestionClient::new(sensor)
            .with_context(|| format!("sensor {:?}: invalid HTTP client", sensor.display_name()))?;
        let queue = Arc::new(RequestQueue::default());
        let stats = Arc::new(SensorStats::default());
        let route = Route {
            queue: Arc::clone(&queue),
            stats: Arc::clone(&stats),
        };
        routes.insert(sensor.data_tag, route.clone());
        if sensor.gust {
            routes.insert(sensor.data_tag + 1, route);
        }
        senders.push((
            sensor.display_name().to_owned(),
            sensor.id.to_ascii_uppercase(),
            queue,
            stats,
            client,
        ));
    }

    let port = open_serial(&config).with_context(|| {
        format!(
            "cannot open serial device {} at startup; it may be missing, inaccessible, or already in use",
            config.serial.port
        )
    })?;
    let running = Arc::new(AtomicBool::new(true));
    let signal_running = Arc::clone(&running);
    ctrlc::set_handler(move || signal_running.store(false, Ordering::SeqCst))
        .context("cannot install SIGINT/SIGTERM handler")?;

    info!(
        "starting config={} serial={}@{} sensors={}",
        path.display(),
        config.serial.port,
        config.serial.baud_rate,
        config.sensors.len()
    );

    let mut workers = Vec::with_capacity(senders.len());
    for (name, id, queue, stats, client) in senders {
        let worker_name = name.clone();
        let worker_queue = Arc::clone(&queue);
        let worker_stats = Arc::clone(&stats);
        let worker_running = Arc::clone(&running);
        let handle = thread::Builder::new()
            .name(format!("sender-{name}"))
            .spawn(move || {
                sender_loop(
                    &worker_name,
                    &worker_queue,
                    &worker_stats,
                    &worker_running,
                    |request| client.send(request),
                )
            })
            .with_context(|| format!("cannot start sender worker for {name:?}"))?;
        workers.push(SenderWorker {
            name,
            id,
            queue,
            stats,
            handle,
        });
    }

    let reader_config = Arc::clone(&config);
    let reader_running = Arc::clone(&running);
    let reader_serial_health = Arc::clone(&serial_health);
    let reader = thread::Builder::new()
        .name("serial-reader".into())
        .spawn(move || {
            reader_loop(
                reader_config,
                port,
                routes,
                reader_serial_health,
                reader_running,
            )
        })
        .context("cannot start serial reader")?;

    let mut last_health = Instant::now();
    let unexpected = loop {
        if !running.load(Ordering::SeqCst) {
            break None;
        }
        if let Some(worker) = finished_worker(&reader, &workers) {
            break Some(worker);
        }
        if last_health.elapsed() >= HEALTH_INTERVAL {
            log_health(&workers, &serial_health);
            last_health = Instant::now();
        }
        thread::sleep(Duration::from_millis(100));
    };

    running.store(false, Ordering::SeqCst);
    let reader_panicked = reader.join().is_err();
    if reader_panicked {
        error!("serial reader panicked");
    }
    for worker in &workers {
        worker.queue.close();
    }

    let mut undelivered = 0usize;
    let mut panicked = false;
    for worker in workers {
        let pending = worker.queue.len();
        let in_flight = match worker.handle.join() {
            Ok(in_flight) => in_flight,
            Err(_) => {
                panicked = true;
                error!("sender worker {:?} panicked", worker.name);
                0
            }
        };
        let lost = pending + in_flight;
        if lost > 0 {
            warn!("sensor {:?}: {lost} undelivered request(s)", worker.name);
        }
        log_sensor_health(&worker.name, &worker.id, &worker.queue, &worker.stats);
        undelivered += lost;
    }

    log_serial_health(&serial_health);
    info!(
        "stopped uptime={:?}; undelivered requests={undelivered}",
        started.elapsed()
    );
    if let Some(worker) = unexpected {
        bail!("{worker} stopped unexpectedly");
    }
    if reader_panicked || panicked {
        bail!("one or more workers panicked");
    }
    Ok(())
}

fn finished_worker(reader: &JoinHandle<()>, workers: &[SenderWorker]) -> Option<String> {
    if reader.is_finished() {
        return Some("serial reader".to_string());
    }
    workers
        .iter()
        .find(|worker| worker.handle.is_finished())
        .map(|worker| format!("sender worker {:?}", worker.name))
}

fn log_health(workers: &[SenderWorker], serial_health: &Mutex<SerialHealth>) {
    for worker in workers {
        log_sensor_health(&worker.name, &worker.id, &worker.queue, &worker.stats);
    }
    log_serial_health(serial_health);
}

fn log_sensor_health(name: &str, id: &str, queue: &RequestQueue, stats: &SensorStats) {
    let health = stats.snapshot();
    let Some(last_reading) = health.last_reading else {
        info!(
            "sensor={name:?} id={id}: health received={} queued={} sent={} suppressed={} retries={} permanent_failures={} dropped={} queue={} last_reading=never rssi=unknown cv=unknown battery=unknown",
            health.received,
            health.queued,
            health.sent,
            health.suppressed,
            health.retries,
            health.permanent_failures,
            health.dropped,
            queue.len(),
        );
        return;
    };
    info!(
        "sensor={name:?} id={id}: health received={} queued={} sent={} suppressed={} retries={} permanent_failures={} dropped={} queue={} last_reading={:?} rssi={}dBm cv={} battery={}",
        health.received,
        health.queued,
        health.sent,
        health.suppressed,
        health.retries,
        health.permanent_failures,
        health.dropped,
        queue.len(),
        last_reading.elapsed(),
        health.rssi_dbm,
        health.cv,
        match health.battery_low {
            Some(true) => "low",
            Some(false) => "ok",
            None => "unknown",
        }
    );
}

fn log_serial_health(serial_health: &Mutex<SerialHealth>) {
    let health = *serial_health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    info!(
        "serial health valid_readings={} discarded_bytes={} unconfigured_frames={} reconnects={}",
        health.valid_readings,
        health.discarded_bytes,
        health.unconfigured_frames,
        health.reconnects
    );
}

fn open_serial(config: &Config) -> serialport::Result<Box<dyn serialport::SerialPort>> {
    let builder =
        serialport::new(&config.serial.port, config.serial.baud_rate).timeout(SERIAL_TIMEOUT);
    #[cfg(unix)]
    let builder = builder.exclusive(true);
    builder.open()
}

fn reader_loop(
    config: Arc<Config>,
    mut port: Box<dyn serialport::SerialPort>,
    routes: HashMap<u16, Route>,
    serial_health: Arc<Mutex<SerialHealth>>,
    running: Arc<AtomicBool>,
) {
    let mut policy = HashMap::<u16, PolicyState>::new();
    while running.load(Ordering::SeqCst) {
        let result = run_reader_until(
            &mut port,
            &config,
            || running.load(Ordering::SeqCst),
            |event| {
                if !running.load(Ordering::SeqCst) {
                    return;
                }
                match event {
                    ReaderEvent::Reading(reading) => {
                        serial_health
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .valid_readings += 1;
                        debug!("received {reading}");
                        let tag = match reading.kind {
                            ReadingKind::WindAverage => reading.sensor.data_tag,
                            ReadingKind::WindGust => reading.sensor.data_tag + 1,
                        };
                        let route = routes
                            .get(&tag)
                            .expect("validated sensor tag must have a sender route");
                        let (first, battery_transition) = route.stats.record_reading(
                            reading.rssi_dbm,
                            reading.cv,
                            reading.battery_low,
                        );
                        if first {
                            info!(
                                "sensor={:?} id={}: first reading kind={} rssi={}dBm cv={} battery={}",
                                reading.sensor.display_name(),
                                reading.sensor.id.to_ascii_uppercase(),
                                reading.kind,
                                reading.rssi_dbm,
                                reading.cv,
                                if reading.battery_low { "low" } else { "ok" }
                            );
                        }
                        match battery_transition {
                            Some(true) => warn!(
                                "sensor={:?} id={}: battery low",
                                reading.sensor.display_name(),
                                reading.sensor.id.to_ascii_uppercase()
                            ),
                            Some(false) => info!(
                                "sensor={:?} id={}: battery recovered",
                                reading.sensor.display_name(),
                                reading.sensor.id.to_ascii_uppercase()
                            ),
                            None => {}
                        }
                        if let Some(reading) = policy
                            .entry(tag)
                            .or_default()
                            .observe(reading, &config.aggregation)
                        {
                            let request = IngestionRequest::from(&reading);
                            debug!(
                                "queued sensor={} kind={}",
                                request.sensor_identifier, reading.kind
                            );
                            let dropped = route.queue.push(request);
                            {
                                let mut health = route.stats.lock();
                                health.queued += 1;
                                if dropped.is_some() {
                                    health.dropped += 1;
                                }
                            }
                            if let Some(dropped) = dropped {
                                warn!(
                                    "sensor={}: queue full; dropped oldest request observed_at={}",
                                    dropped.sensor_identifier, dropped.observed_at
                                );
                            }
                        } else {
                            route.stats.lock().suppressed += 1;
                            debug!("suppressed tag={tag:04X} by send policy");
                        }
                    }
                    ReaderEvent::Drained(bytes) => {
                        serial_health
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .discarded_bytes += bytes as u64;
                        debug!("serial resync discarded {bytes} byte(s)")
                    }
                    ReaderEvent::Unconfigured(frame) => {
                        serial_health
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .unconfigured_frames += 1;
                        debug!("dropped unconfigured frame tag={:04X}", frame.tag)
                    }
                    ReaderEvent::Eof => warn!("serial input ended"),
                }
            },
        );

        if !running.load(Ordering::SeqCst) {
            return;
        }
        if let Err(error) = result {
            warn!("serial read error: {error}");
        }

        drop(port);
        let disconnected_at = Instant::now();
        let mut attempts = 0u64;
        loop {
            if !running.load(Ordering::SeqCst) {
                return;
            }
            attempts += 1;
            match open_serial(&config) {
                Ok(new_port) => {
                    port = new_port;
                    serial_health
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .reconnects += 1;
                    info!(
                        "serial device {} reconnected after {:?} attempts={attempts}",
                        config.serial.port,
                        disconnected_at.elapsed()
                    );
                    break;
                }
                Err(error) => {
                    if should_log_reconnect_failure(attempts) {
                        warn!(
                            "cannot open {}: {error}; outage={:?} attempts={attempts}",
                            config.serial.port,
                            disconnected_at.elapsed()
                        );
                    }
                    sleep_while_running(&running, SERIAL_TIMEOUT);
                }
            }
        }
    }
}

fn sleep_while_running(running: &AtomicBool, duration: Duration) {
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while running.load(Ordering::SeqCst) && slept < duration {
        let remaining = duration - slept;
        let next = step.min(remaining);
        thread::sleep(next);
        slept += next;
    }
}

fn should_log_reconnect_failure(attempts: u64) -> bool {
    attempts == 1 || attempts.is_multiple_of(60)
}

fn sender_loop(
    name: &str,
    queue: &RequestQueue,
    stats: &SensorStats,
    running: &AtomicBool,
    mut send: impl FnMut(&IngestionRequest) -> TransportOutcome,
) -> usize {
    while running.load(Ordering::SeqCst) {
        let Some(request) = queue.pop() else {
            return 0;
        };
        if !running.load(Ordering::SeqCst) {
            return 1;
        }

        let mut backoff = RETRY_MIN;
        let mut retries = 0u64;
        let started = Instant::now();
        loop {
            if !running.load(Ordering::SeqCst) {
                return 1;
            }
            match send(&request) {
                TransportOutcome::Delivered => {
                    stats.lock().sent += 1;
                    debug!(
                        "sensor={name:?}: delivered {} {}",
                        request.sensor_identifier, request.observed_at
                    );
                    if retries > 0 {
                        info!(
                            "sensor={name:?} id={}: HTTP recovered after {retries} retries in {:?}",
                            request.sensor_identifier,
                            started.elapsed()
                        );
                    }
                    break;
                }
                TransportOutcome::Permanent(failure) => {
                    stats.lock().permanent_failures += 1;
                    error!(
                        "sensor={name:?} id={}: permanent HTTP failure: {failure}",
                        request.sensor_identifier
                    );
                    break;
                }
                TransportOutcome::Retry {
                    reason,
                    retry_after,
                } => {
                    retries += 1;
                    stats.lock().retries += 1;
                    let delay = retry_delay(retry_after, backoff);
                    let retry_after_capped =
                        retry_after.is_some_and(|retry_after| retry_after > RETRY_MAX);
                    warn!(
                        "sensor={name:?}: retrying {} {} attempt={retries} reason={reason:?} retry_after={retry_after:?} delay={delay:?} retry_after_capped={retry_after_capped}",
                        request.sensor_identifier, request.observed_at,
                    );
                    if !running.load(Ordering::SeqCst) || !queue.wait_retry(delay) {
                        return 1;
                    }
                    backoff = backoff.saturating_mul(2).min(RETRY_MAX);
                }
            }
        }
    }
    0
}

fn retry_delay(retry_after: Option<Duration>, backoff: Duration) -> Duration {
    retry_after.unwrap_or(backoff).min(RETRY_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_safety_gateway::http::{IngestionPayload, RetryReason};

    fn request(sequence: usize) -> IngestionRequest {
        IngestionRequest {
            schema_version: 1,
            sensor_identifier: "ABC123".into(),
            observed_at: sequence.to_string(),
            payload: IngestionPayload {
                kind: ReadingKind::WindAverage,
                value: sequence as f32,
                unit: "m/s",
                window_seconds: 10,
                battery_low: false,
                rssi_dbm: -70,
                cv: 100,
            },
        }
    }

    #[test]
    fn full_queue_drops_oldest_pending_but_not_in_flight() {
        let queue = RequestQueue::default();
        queue.push(request(0));
        let in_flight = queue.pop().unwrap();

        for sequence in 1..=QUEUE_CAPACITY {
            assert!(queue.push(request(sequence)).is_none());
        }
        let dropped = queue.push(request(QUEUE_CAPACITY + 1)).unwrap();

        assert_eq!(in_flight.observed_at, "0");
        assert_eq!(dropped.observed_at, "1");
        assert_eq!(queue.len(), QUEUE_CAPACITY);
        assert_eq!(queue.pop().unwrap().observed_at, "2");
    }

    #[test]
    fn sender_retries_same_request_before_next_fifo_item() {
        let queue = RequestQueue::default();
        let stats = SensorStats::default();
        queue.push(request(1));
        queue.push(request(2));
        let running = AtomicBool::new(true);
        let mut attempts = Vec::new();

        let in_flight = sender_loop("wind", &queue, &stats, &running, |request| {
            attempts.push(request.observed_at.clone());
            if attempts.len() == 1 {
                TransportOutcome::Retry {
                    reason: RetryReason::RateLimited,
                    retry_after: Some(Duration::ZERO),
                }
            } else {
                if request.observed_at == "2" {
                    running.store(false, Ordering::SeqCst);
                }
                TransportOutcome::Delivered
            }
        });

        assert_eq!(attempts, ["1", "1", "2"]);
        assert_eq!(in_flight, 0);
        assert_eq!(stats.snapshot().sent, 2);
        assert_eq!(stats.snapshot().retries, 1);
    }

    #[test]
    fn sensor_stats_track_readings_and_battery_transitions() {
        let stats = SensorStats::default();

        assert_eq!(stats.record_reading(-70, 100, false), (true, None));
        assert_eq!(stats.record_reading(-71, 99, false), (false, None));
        assert_eq!(stats.record_reading(-72, 98, true), (false, Some(true)));
        assert_eq!(stats.record_reading(-69, 101, false), (false, Some(false)));

        let health = stats.snapshot();
        assert_eq!(health.received, 4);
        assert_eq!(health.rssi_dbm, -69);
        assert_eq!(health.cv, 101);
        assert_eq!(health.battery_low, Some(false));
    }

    #[test]
    fn reconnect_failures_log_initially_and_periodically() {
        assert!(should_log_reconnect_failure(1));
        assert!(!should_log_reconnect_failure(2));
        assert!(!should_log_reconnect_failure(59));
        assert!(should_log_reconnect_failure(60));
        assert!(should_log_reconnect_failure(120));
    }

    #[test]
    fn retry_delay_never_exceeds_maximum() {
        assert_eq!(
            retry_delay(None, Duration::from_secs(32)),
            Duration::from_secs(32)
        );
        assert_eq!(
            retry_delay(Some(Duration::from_secs(30)), Duration::from_secs(1)),
            Duration::from_secs(30)
        );
        assert_eq!(
            retry_delay(Some(Duration::from_secs(3600)), Duration::from_secs(1)),
            RETRY_MAX
        );
    }

    #[test]
    fn shutdown_stops_before_retry_delay_and_reports_in_flight() {
        let queue = Arc::new(RequestQueue::default());
        let stats = Arc::new(SensorStats::default());
        queue.push(request(1));
        let running = Arc::new(AtomicBool::new(true));
        let worker_queue = Arc::clone(&queue);
        let worker_stats = Arc::clone(&stats);
        let worker_running = Arc::clone(&running);
        let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            sender_loop(
                "wind",
                &worker_queue,
                &worker_stats,
                &worker_running,
                |_| {
                    attempted_tx.send(()).unwrap();
                    TransportOutcome::Retry {
                        reason: RetryReason::Network("offline".into()),
                        retry_after: Some(Duration::from_secs(60)),
                    }
                },
            )
        });

        attempted_rx.recv().unwrap();
        running.store(false, Ordering::SeqCst);
        queue.close();
        assert_eq!(worker.join().unwrap(), 1);
    }

    #[test]
    fn finished_sender_is_detected_as_unexpected() {
        let reader_running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&reader_running);
        let reader = thread::spawn(move || {
            while thread_running.load(Ordering::SeqCst) {
                thread::yield_now();
            }
        });
        let workers = vec![SenderWorker {
            name: "wind".into(),
            id: "ABC123".into(),
            queue: Arc::new(RequestQueue::default()),
            stats: Arc::new(SensorStats::default()),
            handle: thread::spawn(|| panic!("worker failed")),
        }];
        while !workers[0].handle.is_finished() {
            thread::yield_now();
        }

        assert_eq!(
            finished_worker(&reader, &workers).as_deref(),
            Some("sender worker \"wind\"")
        );

        reader_running.store(false, Ordering::SeqCst);
        reader.join().unwrap();
        assert!(workers.into_iter().next().unwrap().handle.join().is_err());
    }
}
