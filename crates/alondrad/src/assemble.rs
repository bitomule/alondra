//! The daemon is put together here from its edges: everything outside the process (GitHub, the
//! push relay, the Mac, the SDK host's process, the network) comes in through [`Seams`], and
//! everything inside is the production code. The daemon in production assembles with
//! `alondrad_production`'s seams, the only ones that reach the real GitHub; a scenario assembles
//! the same daemon with edges it controls.
use super::*;
use alondra_agent_runtime::owned_host::Launcher;
use alondra_daemon_engine::domain_github::PrObserver;
use alondra_daemon_engine::domain_merges::PrMerger;
use alondra_engine_ports::machine::MachineProbe;
use alondra_daemon_engine::pr_images::GitHubImages;
use alondra_engine_ports::github::GitHub;
pub use crate::client_routes::PrView;
pub use crate::push_transport::{DevicePush, PushTransport, Pushes};
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

/// What GitHub answers to the daemon: merging, observing, reading a PR and the images of its description.
#[derive(Clone)]
pub struct GithubSeams {
    pub merger: Arc<dyn PrMerger>,
    pub observer: Arc<dyn PrObserver>,
    pub view: Arc<dyn PrView>,
    pub images: Arc<dyn GitHubImages>,
    pub reads: Arc<dyn alondra_engine_ports::github::Gh>,
}

impl GithubSeams {
    /// One object that stands for the whole of GitHub.
    pub fn of<W>(world: Arc<W>) -> Self
    where
        W: PrMerger + PrObserver + alondra_engine_ports::github::Gh + GitHubImages,
    {
        let github = GitHub::of(world.clone());
        Self { merger: github.merger, observer: github.observer, view: Arc::new(client_routes::ViaGh(world)), images: github.images, reads: github.reads }
    }

    /// GitHub as the engine's port sees it.
    pub fn port(&self) -> GitHub {
        GitHub { observer: self.observer.clone(), merger: self.merger.clone(), images: self.images.clone(), reads: self.reads.clone() }
    }

    /// Gives the registry this GitHub, as the daemon does before it serves.
    pub fn wire(&self, registry: &alondra_engine_ports::Registry) {
        registry.provide(Arc::new(self.port()));
    }
}

/// Where the SDK backend comes from.
pub enum Backend {
    /// The host the installed release brought, or the home's own configuration.
    Resolve,
    /// This backend, or none.
    Given(Option<sdk_owner::backend::SdkBackend>),
}

/// Whether the daemon touches what is outside its home: the user's Claude installation, the
/// network, the updater and the reports it sends.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum World {
    Real,
    Isolated,
}

pub struct Seams {
    pub github: GithubSeams,
    pub push: Pushes,
    /// The Mac's gauges in place of the real ones, which `wire_machine` registers.
    pub machine: Option<Arc<dyn MachineProbe>>,
    /// Observes a completed publication of the machine gauges.
    pub machine_published: Option<Arc<dyn Fn() + Send + Sync>>,
    pub launcher: Launcher,
    pub backend: Backend,
    pub world: World,
    /// The `alondra` CLI the daemon runs for the user's answers about held crash reports.
    pub own_cli: Option<std::path::PathBuf>,
    /// The home of the user whose projects the daemon sweeps for open pull requests.
    pub user_home: std::path::PathBuf,
    /// The socket the daemon serves on, bound by whoever picked the port: nothing else can take
    /// the port between picking it and serving on it. The daemon serves on a clone of it.
    pub listener: Option<std::net::TcpListener>,
}

impl Seams {
    /// The edges of the daemon on the user's Mac, with `github` as GitHub.
    pub fn over(github: GithubSeams) -> Self {
        Self {
            github,
            push: Pushes::default(),
            machine: None,
            machine_published: None,
            launcher: Launcher::default(),
            backend: Backend::Resolve,
            world: World::Real,
            own_cli: alondra_host::brew::own_cli(),
            user_home: options::user_home(),
            listener: None,
        }
    }
}

/// Every task the daemon started, so that a daemon torn down leaves nothing running.
#[derive(Clone, Default)]
pub struct Tasks(Arc<Mutex<Vec<tokio::task::AbortHandle>>>);

impl Tasks {
    pub fn spawn<T: Send + 'static>(&self, task: impl Future<Output = T> + Send + 'static) -> tokio::task::JoinHandle<T> {
        let handle = tokio::spawn(task);
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(handle.abort_handle());
        handle
    }

    pub fn abort(&self) {
        for task in self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).drain(..) {
            task.abort();
        }
    }
}

type Serving = Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>;

/// A daemon that is serving, with the loops the supervisor watches.
pub struct Assembled {
    client: Arc<client_routes::Client>,
    gauges: machine::Gauges,
    pub addr: SocketAddr,
    pub domain: Arc<domain_runtime::DomainRuntime>,
    pub auth: Arc<auth::Auth>,
    pub tasks: Tasks,
    pub(crate) server: Serving,
    pub(crate) outbox: tokio::task::JoinHandle<Result<()>>,
    pub(crate) scheduler: tokio::task::JoinHandle<Result<()>>,
    pub(crate) transport: tokio::task::JoinHandle<Result<()>>,
    pub(crate) github: tokio::task::JoinHandle<Result<()>>,
    pub(crate) orchestrator: Option<tokio::task::JoinHandle<Result<()>>>,
    pub(crate) connector: Option<tokio::task::JoinHandle<()>>,
    pub(crate) machine_watch: tokio::task::JoinHandle<()>,
    pub(crate) gate: tokio::task::JoinHandle<()>,
    service: Option<Arc<service::Service>>,
}

impl Assembled {
    pub fn clear_scenario_gauges(&self) {
        self.client.clear_scenario_gauges();
    }

    /// The gauges as the daemon holds its accounts now, for a scenario that settled: the latest
    /// sample takes the accounts again and the gauges the views kept are dropped, so the next view
    /// builds them from that.
    pub fn settle_scenario_gauges(&self) {
        machine::resample(&self.gauges, &*self.domain);
        self.client.clear_scenario_gauges();
    }
    pub async fn shared_tui_feed_work_for_test(&self, secret: &[u8; 32], device: &str, first_timestamp: i64) -> std::result::Result<(u64, usize), String> {
        self.client.shared_tui_feed_work_for_test(&self.auth, secret, device, first_timestamp).await
    }

    /// The phone registers `binding` as where its pushes go, as the app does it.
    pub async fn register_phone_push(&self, phone: &crate::scenario_phone::Phone, binding: &str) -> Result<()> {
        let service = self.service.as_ref().context("the home has no service: call scenario_phone::prepare before starting")?;
        service.register_phone_push(phone, binding).await
    }

    /// The most nodes that work at once, fixed from now on as the user sets it in Settings.
    pub fn limit_nodes(&self, count: u32) {
        self.domain.admission().tune(admission::Tuning { spacing: 1_000, limit: Some(admission::Limit::Fixed(count)) });
        self.domain.admission().poke();
    }
}

/// Opens the domain at `config.home` and assembles the daemon over it: what a scenario starts.
pub async fn start(config: &Config, seams: Seams) -> Result<Assembled> {
    start_over(config, crate::wiring::open(&config.home, &alondra_store_sqlite::Sqlite)?, seams).await
}

/// `start` over a domain the caller opened at `config.home`, so it can prepare the domain before
/// the daemon takes the SDK hosts over: how a scenario's restart keeps the hosts it plays.
pub async fn start_over(config: &Config, domain: domain_runtime::DomainRuntime, seams: Seams) -> Result<Assembled> {
    crate::wire_machine(domain.ports());
    let state = domain_bootstrap::DomainDaemon::over(&config.home, domain)?;
    assemble(config, state, seams).await
}

/// Opens the service, takes the SDK hosts over and starts the daemon on `config`'s port, with
/// `seams` as its edges.
pub async fn assemble(config: &Config, state: Arc<domain_bootstrap::DomainDaemon>, mut seams: Seams) -> Result<Assembled> {
    let auth = state.auth();
    let real = seams.world == World::Real;
    let service = service::Service::open(&config.home, config.port, auth.clone())?.map(|service| {
        Arc::new(
            service
                .with_domain(state.domain())
                .with_client(state.client())
                .with_push_transport(seams.push.clone()),
        )
    });
    if let Some(service) = &service {
        service.recover_interrupted()?;
        state.attach_service(service.clone());
    }
    let actions = views::action_results::recover_interrupted(&config.home)?;
    if actions > 0 {
        log!("the previous daemon stopped in the middle of {actions} local action(s); they are marked interrupted");
    }
    let domain = domain_runtime::DomainRuntime::shared(state.domain());
    if real {
        // Before any SDK execution is taken over: a host that replaces one that ended loads this plugin.
        match alondra_system::plugins::node_source(&watcher::claude_home()).zip(alondra_host::brew::own_cli()) {
            Some((source, cli)) => match alondra_system::plugins::install_node(&config.home, &source, &cli) {
                Ok(dir) => log!("node plugin {} brought up to date from {}", dir.display(), source.display()),
                Err(error) => log!("could not bring the node plugin up to date from {}: {error:#}", source.display()),
            },
            None => log!("Alondra's node plugin was not found: run `alondra doctor --fix`"),
        }
        match alondra_system::plugins::orchestrator_source(&watcher::claude_home()).zip(alondra_host::brew::own_cli()) {
            Some((source, cli)) => {
                if let Err(error) = alondra_system::plugins::install_orchestrator(&config.home, &source, &cli) {
                    log!("Could not update Alondra's orchestrator integration assets: {error:#}");
                }
            }
            None => log!("Alondra's orchestrator assets are missing; run alondra doctor --fix"),
        }
    }
    let backend = match std::mem::replace(&mut seams.backend, Backend::Given(None)) {
        Backend::Given(backend) => backend,
        Backend::Resolve => {
            let environment: std::collections::BTreeMap<String, String> = std::env::vars().collect();
            let daemon = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("alondrad"));
            match sdk_owner::backend::resolve(&config.home, config.sandbox, &options::user_home(), &daemon, &environment) {
                Ok(mut backend) => {
                    backend.native_environment.insert("ALONDRA_PORT".into(), config.port.to_string());
                    log!("the Agent SDK host is {} (configuration {})", backend.host.display(), backend.configuration_digest());
                    Some(backend)
                }
                Err(error) => {
                    log!("no task or orchestrator batch starts: {error:#}");
                    None
                }
            }
        }
    };
    alondra_agent_runtime::wire(domain.ports(), seams.launcher.clone());
    accounts::begin_initial_check(&*domain);
    let recovery = prepare_workers(config, domain.clone(), backend);
    assemble_with(config, state, domain, recovery, service, &seams).await
}

/// `assemble` once the service and the recovery of the hosts are in hand.
pub(crate) async fn assemble_with(
    config: &Config,
    state: Arc<domain_bootstrap::DomainDaemon>,
    domain: Arc<domain_runtime::DomainRuntime>,
    recovery: impl Future<Output = PreparedWorkers>,
    service: Option<Arc<service::Service>>,
    seams: &Seams,
) -> Result<Assembled> {
    crate::wire(domain.ports());
    let tasks = Tasks::default();
    seams.github.wire(domain.ports());
    if let Some(machine) = seams.machine.clone() {
        domain.ports().provide::<dyn MachineProbe>(machine);
    }
    let auth = state.auth();
    let state_domain = domain.clone();
    let state_auth = auth.clone();
    let gauges = state.gauges();
    let client = state.client();
    let push = Arc::new(push::Push::open(&config.home)?);
    let addr = SocketAddr::new(config.listen, config.port);
    let listener = match &seams.listener {
        Some(bound) => {
            let clone = bound.try_clone().with_context(|| format!("cannot share the listener on {addr}"))?;
            clone.set_nonblocking(true)?;
            tokio::net::TcpListener::from_std(clone)?
        }
        None => tokio::net::TcpListener::bind(addr).await.with_context(|| format!("cannot listen on {addr}"))?,
    };
    // The producer transport owns the per-home hook socket before any outbox
    // message can be claimed for a session.
    let hooks = alondra_system::hook_socket::bind(&config.home).await?;
    let real = seams.world == World::Real;
    if let Some(service) = service.clone().filter(|service| real && service.direct_config().enabled) {
        let direct = Arc::new(direct::Direct::new(
            service.clone(),
            Arc::new(direct::pairing::PairingBook::new(&config.home)),
            config.name.clone(),
            direct_approval(config.home.clone(), auth.clone(), service),
        ));
        state.attach_direct(direct.clone());
        tasks.spawn(direct::run(direct, config.sandbox));
    }
    if let Some(service) = service.clone().filter(|service| real && service.iroh_config().enabled) {
        tasks.spawn(iroh_endpoint::run(service));
        tasks.spawn(local_network::watch(alondra_system::local_network::probe));
        tasks.spawn(local_network_card::run(domain.clone()));
    }
    tasks.spawn(device_expiry::run(state.clone(), config.home.clone(), config.device_expiry_days));
    let app = domain_bootstrap::router(state)
        .layer(axum::middleware::from_fn_with_state(auth, auth::guard))
        .layer(axum::middleware::from_fn(views::stamp_version));
    log!(
        "domain listener on http://{addr}, data in {}",
        config.home.display()
    );
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .into_future();
    let serving = tasks.spawn(server);
    let server = async move { serving.await.unwrap_or_else(|error| Err(std::io::Error::other(error))) };
    let PreparedWorkers(domain) = recovery.await;
    accounts::end_abandoned_sign_ins(&*domain);
    tasks.spawn(usage_pause::run(
        domain.clone(),
        push_alerts::PushAlerts { domain: domain.clone(), push: push.clone(), service: service.clone(), client: client.clone() },
    ));
    tasks.spawn(claude_access::run(domain.clone()));
    tasks.spawn(accounts::run(domain.clone(), config.home.clone()));
    tasks.spawn(usage_probe::run(domain.clone(), config.home.clone()));
    tasks.spawn(mcp_waits::run(domain.clone()));
    let machine_watch = tasks.spawn(machine_watch::run(
        domain.clone(),
        gauges.clone(),
        push_alerts::PushAlerts { domain: domain.clone(), push: push.clone(), service: service.clone(), client: client.clone() },
    ));
    let outbox = tasks.spawn(domain_delivery::run(
        domain.clone(),
        config.home.clone(),
        push,
        service.clone(),
        client.clone(),
    ));
    if let Some(service) = &service {
        tasks.spawn(push_retract::run(domain.clone(), service.clone()));
        tasks.spawn(activity_push::run(domain.clone(), service.clone(), client.clone()));
    }
    let scheduler = tasks.spawn(domain_schedule_runner::run(
        domain.clone(),
        config.home.clone(),
        config.wake,
    ));
    if config.wake {
        tasks.spawn(auto_launch::run(domain.clone(), config.home.clone()));
    }
    let handed = domain.clone();
    alondra_core::clock::spawn_blocking_unwatched(move || domain_orchestrator::read_handed_notices_in_background(&handed));
    tasks.spawn(retention::run(domain.clone()));
    let gate = tasks.spawn(admission::run(domain.clone(), config.home.clone()));
    let transport = tasks.spawn(domain_transport::run(hooks, domain.clone()));
    if real && accounts::config(&config.home).is_ok_and(|accounts| accounts.connected_providers().contains(&alondra_core::accounts::Provider::Claude)) {
        tasks.spawn(claude_check::run(alondra_system::plugins::node()));
    }
    tasks.spawn(bg_handover::run(state_domain.clone()));
    tasks.spawn(account_move::run(state_domain.clone()));
    tasks.spawn(provider_transition::run(state_domain.clone()));
    tasks.spawn(obligation_watch::run(state_domain.clone()));
    tasks.spawn(alondra_daemon_engine::report_held_card::run(domain.clone(), config.home.clone(), seams.own_cli.clone(), config.fast_timing));
    let release = if config.fast_timing { &idle_release::Timing::SANDBOX } else { &idle_release::Timing::LIVE };
    tasks.spawn(idle_release::run(state_domain.clone(), release));
    let closer = if config.fast_timing { task_closer::Timing::sandbox(config.close_free_nodes_after) } else { task_closer::Timing::new(config.close_free_nodes_after) };
    tasks.spawn(task_closer::run(state_domain.clone(), client.sessions(), closer));
    let merges_domain = domain.clone();
    let (merger, pr_poll) = if config.fast_timing {
        (domain_merges::Merger::with(std::time::Duration::from_secs(2), std::time::Duration::from_secs(15 * 60)), std::time::Duration::from_secs(2))
    } else {
        (domain_merges::Merger::default(), std::time::Duration::from_secs(60))
    };
    tasks.spawn(alondra_daemon_engine::domain_verdicts::run(merges_domain.clone(), Arc::new(seams.github.merger.clone()), pr_poll));
    tasks.spawn(domain_merges::run(merges_domain, Arc::new(seams.github.merger.clone()), merger));
    tasks.spawn(alondra_daemon_engine::open_pr_sweep::run(domain.clone(), config.home.clone(), seams.user_home.clone()));
    let github = tasks.spawn(domain_github::run(
        domain,
        Arc::new(seams.github.observer.clone()),
        pr_poll,
    ));
    let machine_gauges = gauges.clone();
    tasks.spawn(machine::run(gauges, state_domain.clone(), seams.machine_published.clone()));
    let owners = state_domain.clone();
    client.use_pr_view(seams.github.view.clone());
    client.use_pr_images(seams.github.images.clone());
    client.use_pr_merger(seams.github.merger.clone());
    let observed_client = client.clone();
    tasks.spawn(client.run(seams.github.view.clone()));
    let orchestrator = config.wake.then(|| {
        tasks.spawn(domain_orchestrator::run(
            state_domain,
            config.home.clone(),
            watcher::claude_home(),
        ))
    });
    let kept = service.clone();
    let connector = service
        .filter(|service| real && service.has_relay())
        .map(|service| tasks.spawn(async move { service.run().await }));
    if real {
        tasks.spawn(updates::updater(config).run());
        tasks.spawn(reports::retry_pending(config.home.clone(), alondra_host::brew::own_cli(), config.fast_timing));
    }
    Ok(Assembled {
        client: observed_client,
        gauges: machine_gauges,
        addr,
        domain: owners,
        auth: state_auth,
        tasks,
        server: Box::pin(server),
        outbox,
        scheduler,
        transport,
        github,
        orchestrator,
        connector,
        machine_watch,
        gate,
        service: kept,
    })
}
