use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::Client;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Responding,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct ViewFailure {
    pub error: anyhow::Error,
    pub availability: Availability,
}

#[derive(Default)]
struct State {
    responded: bool,
    finished: bool,
}

struct Observation {
    deadline: Option<Instant>,
    state: Mutex<State>,
    changed: Condvar,
}

/// Evidence belongs to one request, including failures while reading its response body.
#[derive(Clone)]
pub(crate) struct ResponseObserver(Arc<Observation>);

impl Default for ResponseObserver {
    fn default() -> Self { Self::new(None) }
}

impl ResponseObserver {
    fn new(deadline: Option<Instant>) -> Self {
        Self(Arc::new(Observation { deadline, state: Mutex::new(State::default()), changed: Condvar::new() }))
    }

    pub(crate) fn begin_request(&self) {
        self.0.state.lock().unwrap_or_else(|poison| poison.into_inner()).responded = false;
    }

    pub(crate) fn received(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if self.0.deadline.is_none_or(|deadline| Instant::now() < deadline) {
            state.responded = true;
        }
        self.0.changed.notify_all();
    }

    pub(crate) fn responded(&self) -> bool {
        self.0.state.lock().unwrap_or_else(|poison| poison.into_inner()).responded
    }

    fn finished(&self) -> bool {
        self.0.state.lock().unwrap_or_else(|poison| poison.into_inner()).finished
    }

    fn finish(&self) {
        self.0.state.lock().unwrap_or_else(|poison| poison.into_inner()).finished = true;
        self.0.changed.notify_all();
    }

    fn wait(&self) -> Availability {
        let deadline = self.0.deadline.expect("a probe has a deadline");
        let mut state = self.0.state.lock().unwrap_or_else(|poison| poison.into_inner());
        loop {
            if state.responded { return Availability::Responding; }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if state.finished || remaining.is_zero() { return Availability::Unavailable; }
            state = self.0.changed.wait_timeout(state, remaining)
                .unwrap_or_else(|poison| poison.into_inner()).0;
        }
    }
}

/// Shared by the feed and main-view requests for one TUI destination. A transport that outlives
/// the decision deadline retains its slot, so retries cannot accumulate blocked workers.
#[derive(Clone, Default)]
pub(crate) struct AvailabilityProbe(Arc<Mutex<Option<ResponseObserver>>>);

impl AvailabilityProbe {
    fn start(&self, deadline: Instant, work: impl FnOnce(ResponseObserver) + Send + 'static) -> ResponseObserver {
        let mut slot = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(active) = slot.as_ref().filter(|active| !active.finished()) {
            if active.0.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let expired = ResponseObserver::new(Some(deadline));
                expired.finish();
                return expired;
            }
            return active.clone();
        }
        let observer = ResponseObserver::new(Some(deadline));
        *slot = Some(observer.clone());
        let worker = observer.clone();
        if std::thread::Builder::new().name("alondra-availability".into()).spawn(move || {
            struct Finish(ResponseObserver);
            impl Drop for Finish {
                fn drop(&mut self) { self.0.finish(); }
            }
            let _finish = Finish(worker.clone());
            work(worker.clone());
        }).is_err() {
            observer.finish();
        }
        observer
    }

    fn check(&self, client: &Client) -> Availability {
        let deadline = Instant::now() + PROBE_TIMEOUT;
        let mut client = client.bounded(PROBE_TIMEOUT);
        self.start(deadline, move |observer| {
            client.response_observer = Some(observer.clone());
            let path = client.feed_path(None, &[], 0);
            if client.exchange("GET", &path, None).is_ok() { observer.received(); }
        }).wait()
    }
}

impl Client {
    pub(crate) fn checked<T>(&self, probe: &AvailabilityProbe, operation: impl FnOnce(&Client) -> anyhow::Result<T>) -> Result<T, ViewFailure> {
        let observer = ResponseObserver::default();
        let mut client = self.clone();
        client.response_observer = Some(observer.clone());
        operation(&client).map_err(|error| ViewFailure {
            error,
            availability: if observer.responded() { Availability::Responding } else { probe.check(self) },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn request(stream: &std::net::TcpStream) -> String {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" { break; }
        }
        first
    }

    #[test]
    fn a_timed_out_operation_probes_the_same_destination_without_views_or_waiting() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let line = request(&stream);
            let path = line.split_whitespace().nth(1).unwrap();
            let url = url::Url::parse(&format!("http://fixture{path}")).unwrap();
            let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            assert_eq!(url.path(), "/changes");
            assert_eq!(query["wait"], "0");
            assert!(!query.contains_key("views"));
            assert!(!query.contains_key("cursor"));
            write!(stream, "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let failure = client.checked::<()>(&AvailabilityProbe::default(), |_| {
            Err(ureq::Error::Timeout(ureq::Timeout::Global).into())
        }).unwrap_err();
        server.join().unwrap();
        assert_eq!(failure.availability, Availability::Responding);
        assert!(failure.error.is::<ureq::Error>());
    }

    #[test]
    fn a_later_request_without_a_response_does_not_reuse_http_evidence() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            request(&stream);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
            drop(stream);
            let (stream, _) = listener.accept().unwrap();
            request(&stream);
            drop(listener);
            drop(stream);
        });
        let failure = client.checked(&AvailabilityProbe::default(), |client| {
            client.get::<serde_json::Value>("/first")?;
            client.get::<serde_json::Value>("/second")
        }).unwrap_err();
        server.join().unwrap();
        assert_eq!(failure.availability, Availability::Unavailable);
    }

    #[test]
    fn probe_headers_confirm_connection_before_the_response_body_finishes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
        let (release, blocked) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            request(&stream);
            write!(stream, "HTTP/1.1 500 Error\r\nContent-Length: 100\r\nConnection: close\r\n\r\n").unwrap();
            stream.flush().unwrap();
            blocked.recv().unwrap();
        });
        let failure = client.checked::<()>(&AvailabilityProbe::default(), |_| {
            Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset).into())
        }).unwrap_err();
        release.send(()).unwrap();
        server.join().unwrap();
        assert_eq!(failure.availability, Availability::Responding);
    }

    #[test]
    fn a_response_with_invalid_view_data_still_proves_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            request(&stream);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnull").unwrap();
        });
        let failure = client.checked(&AvailabilityProbe::default(), |client| client.views("home", None)).unwrap_err();
        server.join().unwrap();
        assert_eq!(failure.availability, Availability::Responding);
    }

    #[test]
    fn a_redirect_is_a_response_from_the_original_destination() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            request(&stream);
            write!(stream, "HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let failure = client.checked(&AvailabilityProbe::default(), |client| client.views("home", None)).unwrap_err();
        server.join().unwrap();
        assert_eq!(failure.availability, Availability::Responding);
    }

    #[test]
    fn missing_response_preserves_the_operation_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
        drop(listener);
        let failure = client.checked::<()>(&AvailabilityProbe::default(), |_| {
            Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset).into())
        }).unwrap_err();
        assert_eq!(failure.availability, Availability::Unavailable);
        assert_eq!(failure.error.downcast_ref::<std::io::Error>().unwrap().kind(), std::io::ErrorKind::ConnectionReset);
    }

    #[test]
    fn availability_recovers_when_the_same_destination_resumes_answering() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            request(&stream);
            drop(stream);
            let (mut stream, _) = listener.accept().unwrap();
            request(&stream);
            write!(stream, "HTTP/1.1 422 Invalid Input\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let probe = AvailabilityProbe::default();
        let failed_operation = |_: &Client| -> anyhow::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset).into())
        };
        assert_eq!(client.checked(&probe, failed_operation).unwrap_err().availability, Availability::Unavailable);
        assert_eq!(client.checked(&probe, failed_operation).unwrap_err().availability, Availability::Responding);
        server.join().unwrap();
    }

    #[test]
    fn expired_probes_ignore_late_responses_and_retries_share_the_worker_until_it_exits() {
        let probe = AvailabilityProbe::default();
        let (entered, started) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let active = probe.start(Instant::now(), move |observer| {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            observer.received();
        });
        started.recv().unwrap();
        assert_eq!(active.wait(), Availability::Unavailable);
        for _ in 0..10 {
            let reused = probe.start(Instant::now() + PROBE_TIMEOUT, |_| panic!("another worker started"));
            assert_eq!(reused.wait(), Availability::Unavailable);
        }
        release.send(()).unwrap();
        let state = active.0.state.lock().unwrap();
        let state = active.0.changed.wait_while(state, |state| !state.finished).unwrap();
        assert!(!state.responded);
        drop(state);
        let recovered = probe.start(Instant::now() + PROBE_TIMEOUT, |observer| observer.received());
        assert!(!Arc::ptr_eq(&active.0, &recovered.0));
        assert_eq!(recovered.wait(), Availability::Responding);
    }

    #[test]
    fn a_new_failure_cannot_reuse_an_expired_response_while_its_body_is_blocked() {
        let observer = ResponseObserver::new(Some(Instant::now()));
        observer.0.state.lock().unwrap().responded = true;
        let probe = AvailabilityProbe(Arc::new(Mutex::new(Some(observer))));
        let next = probe.start(Instant::now() + PROBE_TIMEOUT, |_| panic!("another worker started"));
        assert_eq!(next.wait(), Availability::Unavailable);
    }
}
