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
use std::time::Duration;

const QUEUE_CAPACITY: usize = 100;
const SERIAL_TIMEOUT: Duration = Duration::from_secs(1);
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);

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
    queue: Arc<RequestQueue>,
    handle: JoinHandle<usize>,
}

pub fn run(config: Config, path: &Path) -> Result<()> {
    let config = Arc::new(config);
    let mut senders = Vec::with_capacity(config.sensors.len());
    let mut routes = HashMap::new();

    for configured in &config.sensors {
        let Sensor::BwWss(sensor) = configured;
        let client = IngestionClient::new(sensor)
            .with_context(|| format!("sensor {:?}: invalid HTTP client", sensor.display_name()))?;
        let queue = Arc::new(RequestQueue::default());
        routes.insert(sensor.data_tag, Arc::clone(&queue));
        if sensor.gust {
            routes.insert(sensor.data_tag + 1, Arc::clone(&queue));
        }
        senders.push((sensor.display_name().to_owned(), queue, client));
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
    for (name, queue, client) in senders {
        let worker_name = name.clone();
        let worker_queue = Arc::clone(&queue);
        let worker_running = Arc::clone(&running);
        let handle = thread::Builder::new()
            .name(format!("sender-{name}"))
            .spawn(move || {
                sender_loop(&worker_name, &worker_queue, &worker_running, |request| {
                    client.send(request)
                })
            })
            .with_context(|| format!("cannot start sender worker for {name:?}"))?;
        workers.push(SenderWorker {
            name,
            queue,
            handle,
        });
    }

    let reader_config = Arc::clone(&config);
    let reader_running = Arc::clone(&running);
    let reader = thread::Builder::new()
        .name("serial-reader".into())
        .spawn(move || reader_loop(reader_config, port, routes, reader_running))
        .context("cannot start serial reader")?;

    let unexpected = loop {
        if !running.load(Ordering::SeqCst) {
            break None;
        }
        if let Some(worker) = finished_worker(&reader, &workers) {
            break Some(worker);
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
        undelivered += lost;
    }

    info!("stopped; undelivered requests={undelivered}");
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
    routes: HashMap<u16, Arc<RequestQueue>>,
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
                        debug!("received {reading}");
                        let tag = match reading.kind {
                            ReadingKind::WindAverage => reading.sensor.data_tag,
                            ReadingKind::WindGust => reading.sensor.data_tag + 1,
                        };
                        if let Some(reading) = policy
                            .entry(tag)
                            .or_default()
                            .observe(reading, &config.aggregation)
                        {
                            let request = IngestionRequest::from(&reading);
                            let queue = routes
                                .get(&tag)
                                .expect("validated sensor tag must have a sender queue");
                            debug!(
                                "queued sensor={} kind={}",
                                request.sensor_identifier, reading.kind
                            );
                            if let Some(dropped) = queue.push(request) {
                                warn!(
                                    "sensor={}: queue full; dropped oldest request observed_at={}",
                                    dropped.sensor_identifier, dropped.observed_at
                                );
                            }
                        } else {
                            debug!("suppressed tag={tag:04X} by send policy");
                        }
                    }
                    ReaderEvent::Drained(bytes) => {
                        debug!("serial resync discarded {bytes} byte(s)")
                    }
                    ReaderEvent::Unconfigured(frame) => {
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
        loop {
            if !running.load(Ordering::SeqCst) {
                return;
            }
            match open_serial(&config) {
                Ok(new_port) => {
                    port = new_port;
                    info!("serial device {} reconnected", config.serial.port);
                    break;
                }
                Err(error) => {
                    warn!("cannot open {}: {error}", config.serial.port);
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

fn sender_loop(
    name: &str,
    queue: &RequestQueue,
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
        loop {
            if !running.load(Ordering::SeqCst) {
                return 1;
            }
            match send(&request) {
                TransportOutcome::Delivered => {
                    debug!(
                        "sensor={name:?}: delivered {} {}",
                        request.sensor_identifier, request.observed_at
                    );
                    break;
                }
                TransportOutcome::Permanent(failure) => {
                    error!("sensor={name:?}: permanent HTTP failure: {failure}");
                    break;
                }
                TransportOutcome::Retry {
                    reason,
                    retry_after,
                } => {
                    let delay = retry_after.unwrap_or(backoff);
                    warn!(
                        "sensor={name:?}: retrying {} {} after {delay:?}: {reason:?}",
                        request.sensor_identifier, request.observed_at
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
        queue.push(request(1));
        queue.push(request(2));
        let running = AtomicBool::new(true);
        let mut attempts = Vec::new();

        let in_flight = sender_loop("wind", &queue, &running, |request| {
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
    }

    #[test]
    fn shutdown_stops_before_retry_delay_and_reports_in_flight() {
        let queue = Arc::new(RequestQueue::default());
        queue.push(request(1));
        let running = Arc::new(AtomicBool::new(true));
        let worker_queue = Arc::clone(&queue);
        let worker_running = Arc::clone(&running);
        let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            sender_loop("wind", &worker_queue, &worker_running, |_| {
                attempted_tx.send(()).unwrap();
                TransportOutcome::Retry {
                    reason: RetryReason::Network("offline".into()),
                    retry_after: Some(Duration::from_secs(60)),
                }
            })
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
            queue: Arc::new(RequestQueue::default()),
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
