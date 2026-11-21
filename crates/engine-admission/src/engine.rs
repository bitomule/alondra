//! What the gate asks of the rest of the engine, through the ports the daemon wires: the owner of
//! the SDK hosts (`SdkHosts`) and the engine (`AdmissionEngine`), each bound to the runtime the gate
//! runs in.
use alondra_core::Millis;
use alondra_domain_rules::{AgentId, DomainError, DomainState, NodeEvent, TaskId};
use alondra_engine_ports::Runtime;
use alondra_engine_ports::admission::{AdmissionEngine, NodeReport};
use alondra_engine_ports::api::ApiWatch;
use alondra_engine_ports::hosts::SdkHosts;
use std::path::Path;
use std::sync::Arc;

pub trait EngineOf {
    fn hosts(&self) -> Hosts<'_>;
    fn engine(&self) -> Engine<'_>;
}

impl<R: Runtime> EngineOf for R {
    fn hosts(&self) -> Hosts<'_> {
        Hosts { runtime: self, port: self.ports().port::<dyn SdkHosts>().expect("the daemon gives the runtime the owner of its SDK hosts") }
    }

    fn engine(&self) -> Engine<'_> {
        Engine { runtime: self, port: self.ports().port::<dyn AdmissionEngine>().expect("the daemon gives the runtime what the admission gate asks of the engine") }
    }
}

pub struct Hosts<'a> {
    runtime: &'a dyn Runtime,
    port: Arc<dyn SdkHosts>,
}

impl Hosts<'_> {
    pub fn owns(&self, agent: &AgentId) -> bool {
        self.port.owns(self.runtime, agent)
    }

    pub fn node_footprints(&self) -> std::io::Result<std::collections::BTreeMap<AgentId, u64>> {
        self.port.node_footprints(self.runtime)
    }

    pub fn halt(&self, task: TaskId, agent: AgentId) {
        self.port.halt(self.runtime, task, agent);
    }

    pub fn take_if_unowned(&self, task: &TaskId) {
        self.port.take_if_unowned(self.runtime, task);
    }

    pub fn take_working_unowned(&self, state: &DomainState, now: Millis) {
        self.port.take_working_unowned(self.runtime, state, now);
    }

    pub fn close_hosts_of_ended_nodes(&self, state: &DomainState) {
        self.port.close_hosts_of_ended_nodes(self.runtime, state);
    }

    pub fn retake_waiting_for_space(&self) {
        self.port.retake_waiting_for_space(self.runtime);
    }
}

pub struct Engine<'a> {
    runtime: &'a dyn Runtime,
    port: Arc<dyn AdmissionEngine>,
}

impl<'a> Engine<'a> {
    pub fn report(&self, session: &str) -> Option<NodeReport> {
        self.port.report(self.runtime, session)
    }

    pub fn turn(&self, session: &str) -> Option<u64> {
        self.port.turn(self.runtime, session)
    }

    pub fn interrupt(&self, session: &str) -> bool {
        self.port.interrupt(self.runtime, session)
    }

    pub fn is_closed(&self, session: &str) -> bool {
        self.port.is_closed(self.runtime, session)
    }

    pub fn is_launching(&self, task: &TaskId) -> bool {
        self.port.is_launching(self.runtime, task)
    }

    pub fn usage_paused(&self) -> bool {
        self.port.usage_paused(self.runtime)
    }

    pub fn api_watch(&self) -> &'a ApiWatch {
        self.port.api_watch(self.runtime)
    }

    pub fn usage_ready(&self, state: &DomainState, task: &TaskId, since: Millis, now: Millis) -> bool {
        self.port.usage_ready(state, task, since, now)
    }

    pub fn tell_node(&self, task: &TaskId, text: &str, command: &str) -> bool {
        self.port.tell_node(self.runtime, task, text, command).is_ok()
    }

    pub fn tell_session(&self, session: &str, text: &str) -> bool {
        self.port.tell_session(self.runtime, session, text, true)
    }

    pub fn node_event(&self, agent: &AgentId, event: NodeEvent) -> Result<bool, String> {
        self.port.node_event(self.runtime, agent, event)
    }

    pub async fn launch(&self, home: &Path, queued: serde_json::Value) -> Result<serde_json::Value, DomainError> {
        self.port.launch(self.runtime, home, queued).await
    }

    pub fn start_ended(&self, task: &TaskId) -> Result<bool, String> {
        self.port.start_ended(self.runtime, task)
    }

    pub fn start_failed(&self, task: &TaskId, error: &DomainError, now: Millis) -> Result<(), String> {
        self.port.start_failed(self.runtime, task, error, now)
    }
}
