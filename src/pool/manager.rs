use anyhow::{anyhow, Result};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::api::handlers::Metrics;
use crate::config::PoolConfig;

use super::health;
use super::lease::{LeaseOutcome, VmLease};
use super::policy::{lifecycle_cap_reason, RecycleReason};
use super::recycler::{recent_recycle_reasons, recycle_label};
use super::types::{
    check_language_known, pool_status_now, LaneSnapshot, PoolEvent, PoolEventType,
    PoolStatusSnapshot, PooledVm, VmFactory,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecycleScope {
    Idle,
    All,
}

#[derive(Default)]
struct PoolLaneState {
    idle: VecDeque<PooledVm>,
    active: usize,
    waiters: usize,
    target_idle: usize,
    quarantined_total: u64,
    borrow_count: u64,
    borrow_latency_ms_sum: f64,
    recycle_generation: u64,
    recycle_counts: HashMap<String, u64>,
    quarantine_counts: HashMap<String, u64>,
}

pub(crate) struct PoolLane {
    language: String,
    config: PoolConfig,
    factory: Arc<dyn VmFactory>,
    metrics: Arc<Metrics>,
    state: Mutex<PoolLaneState>,
    condvar: Condvar,
    events: Arc<Mutex<VecDeque<PoolEvent>>>,
    next_vm_id: Arc<AtomicU64>,
}

impl PoolLane {
    fn max_total(&self) -> Option<usize> {
        if self.config.max_idle_per_lang == 0 {
            None
        } else {
            Some(
                self.config
                    .max_idle_per_lang
                    .max(self.config.min_idle_per_lang),
            )
        }
    }

    fn new_vm(&self) -> Result<PooledVm> {
        let vm = self.factory.create(&self.language)?;
        Ok(PooledVm {
            id: self.next_vm_id.fetch_add(1, Ordering::Relaxed) + 1,
            language: self.language.clone(),
            vm,
            request_count: 0,
            cumulative_exec_ms: 0,
            last_health_probe: Instant::now(),
            timeout_count: 0,
            signal_death_count: 0,
            recycle_generation: self
                .state
                .lock()
                .map(|state| state.recycle_generation)
                .unwrap_or_default(),
        })
    }

    fn push_event(
        &self,
        event_type: PoolEventType,
        reason: &str,
        vm_id: Option<u64>,
        details: String,
    ) {
        let mut events = match self.events.lock() {
            Ok(events) => events,
            Err(poisoned) => poisoned.into_inner(),
        };
        events.push_back(PoolEvent::new(
            self.language.clone(),
            event_type,
            reason.to_string(),
            vm_id,
            details,
        ));
        while events.len() > self.config.event_buffer_size {
            events.pop_front();
        }
    }

    fn record_borrow_latency(&self, started: Instant) {
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.metrics.pool_borrow_time_hist.observe(elapsed_ms);
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.borrow_count += 1;
        state.borrow_latency_ms_sum += elapsed_ms;
    }

    pub(crate) fn finalize_vm(&self, vm: PooledVm, outcome: LeaseOutcome) {
        let release_started = Instant::now();
        let mut maybe_idle = None;
        let mut notify = false;
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.active = state.active.saturating_sub(1);
            let generation_mismatch = vm.recycle_generation < state.recycle_generation;
            match outcome {
                LeaseOutcome::ReturnedIdle => {
                    if generation_mismatch {
                        self.metrics.pool_recycles.fetch_add(1, Ordering::Relaxed);
                        *state
                            .recycle_counts
                            .entry(RecycleReason::Manual.as_str().to_string())
                            .or_insert(0) += 1;
                        self.push_event(
                            PoolEventType::Recycled,
                            RecycleReason::Manual.as_str(),
                            Some(vm.id),
                            "active VM returned after manual recycle-all request".into(),
                        );
                    } else if let Some(reason) = lifecycle_cap_reason(&vm, &self.config) {
                        self.metrics.pool_recycles.fetch_add(1, Ordering::Relaxed);
                        *state
                            .recycle_counts
                            .entry(recycle_label(&reason).to_string())
                            .or_insert(0) += 1;
                        self.push_event(
                            PoolEventType::Recycled,
                            recycle_label(&reason),
                            Some(vm.id),
                            "lifecycle cap reached on lease return".into(),
                        );
                    } else if state.idle.len() < state.target_idle {
                        maybe_idle = Some(vm);
                        notify = true;
                    } else {
                        self.metrics.pool_recycles.fetch_add(1, Ordering::Relaxed);
                        *state
                            .recycle_counts
                            .entry(RecycleReason::IdleOverflow.as_str().to_string())
                            .or_insert(0) += 1;
                        self.push_event(
                            PoolEventType::Recycled,
                            RecycleReason::IdleOverflow.as_str(),
                            Some(vm.id),
                            "idle queue above target".into(),
                        );
                    }
                }
                LeaseOutcome::Recycled { reason } => {
                    self.metrics.pool_recycles.fetch_add(1, Ordering::Relaxed);
                    *state
                        .recycle_counts
                        .entry(recycle_label(&reason).to_string())
                        .or_insert(0) += 1;
                    self.push_event(
                        PoolEventType::Recycled,
                        recycle_label(&reason),
                        Some(vm.id),
                        "lease finalized as recycled".into(),
                    );
                }
                LeaseOutcome::Quarantined { reason } => {
                    self.metrics
                        .pool_quarantines
                        .fetch_add(1, Ordering::Relaxed);
                    state.quarantined_total += 1;
                    *state
                        .quarantine_counts
                        .entry(recycle_label(&reason).to_string())
                        .or_insert(0) += 1;
                    self.push_event(
                        PoolEventType::Quarantined,
                        recycle_label(&reason),
                        Some(vm.id),
                        "lease finalized as quarantined".into(),
                    );
                }
            }
            if let Some(idle_vm) = maybe_idle.take() {
                state.idle.push_back(idle_vm);
                self.push_event(
                    PoolEventType::ReturnedIdle,
                    "returned_idle",
                    state.idle.back().map(|vm| vm.id),
                    "lease returned to idle pool".into(),
                );
            }
        }
        self.metrics
            .lease_release_time_hist
            .observe(release_started.elapsed().as_secs_f64() * 1000.0);
        if notify {
            self.condvar.notify_one();
        }
        self.ensure_idle_target();
    }

    pub fn borrow(self: &Arc<Self>, timeout: Duration) -> Result<VmLease> {
        let started = Instant::now();
        let deadline = Instant::now() + timeout;
        let mut create_error: Option<anyhow::Error> = None;

        loop {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };

            if let Some(vm) = state.idle.pop_front() {
                state.active += 1;
                drop(state);
                self.record_borrow_latency(started);
                self.push_event(
                    PoolEventType::Borrowed,
                    "borrowed",
                    Some(vm.id),
                    "borrowed pooled VM from idle lane".into(),
                );
                return Ok(VmLease::new(self.clone(), vm));
            }

            let total = state.active + state.idle.len();
            let allow_create = match self.max_total() {
                Some(max_total) => total < max_total,
                None => true,
            };
            if allow_create {
                state.active += 1;
                drop(state);
                match self.new_vm() {
                    Ok(vm) => {
                        self.record_borrow_latency(started);
                        self.push_event(
                            PoolEventType::Created,
                            "cold_create",
                            Some(vm.id),
                            "created VM on borrow".into(),
                        );
                        return Ok(VmLease::new(self.clone(), vm));
                    }
                    Err(err) => {
                        let mut state = match self.state.lock() {
                            Ok(state) => state,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        state.active = state.active.saturating_sub(1);
                        create_error = Some(err);
                        continue;
                    }
                }
            }

            let now = Instant::now();
            if now >= deadline {
                self.metrics
                    .pool_borrow_timeouts
                    .fetch_add(1, Ordering::Relaxed);
                self.push_event(
                    PoolEventType::BorrowTimedOut,
                    "borrow_timeout",
                    None,
                    "timed out waiting for an idle VM".into(),
                );
                return Err(create_error.unwrap_or_else(|| anyhow!("pool borrow timed out")));
            }

            let remaining = deadline.saturating_duration_since(now);
            state.waiters += 1;
            let waited = self.condvar.wait_timeout(state, remaining);
            let (mut state, _) = match waited {
                Ok(result) => result,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.waiters = state.waiters.saturating_sub(1);
        }
    }

    pub fn ensure_idle_target(&self) {
        loop {
            let should_create = {
                let state = match self.state.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                let total = state.active + state.idle.len();
                let below_target = state.idle.len() < state.target_idle;
                let below_cap = match self.max_total() {
                    Some(max_total) => total < max_total,
                    None => true,
                };
                below_target && below_cap
            };
            if !should_create {
                break;
            }
            match self.new_vm() {
                Ok(vm) => {
                    let vm_id = vm.id;
                    let mut state = match self.state.lock() {
                        Ok(state) => state,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    state.idle.push_back(vm);
                    self.push_event(
                        PoolEventType::Created,
                        "prefill",
                        Some(vm_id),
                        "created idle VM to satisfy target".into(),
                    );
                    self.condvar.notify_one();
                }
                Err(err) => {
                    self.push_event(
                        PoolEventType::Quarantined,
                        RecycleReason::TransportFailure.as_str(),
                        None,
                        format!("failed to prefill idle VM: {}", err),
                    );
                    break;
                }
            }
        }
    }

    pub fn set_target_idle(&self, requested: usize) -> usize {
        let clamped = requested.max(self.config.min_idle_per_lang).min(
            self.config
                .max_idle_per_lang
                .max(self.config.min_idle_per_lang),
        );
        let mut drained = Vec::new();
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.target_idle = clamped;
            while state.idle.len() > state.target_idle {
                if let Some(vm) = state.idle.pop_back() {
                    drained.push(vm.id);
                }
            }
        }
        for vm_id in drained {
            self.push_event(
                PoolEventType::Recycled,
                RecycleReason::Manual.as_str(),
                Some(vm_id),
                "manual scale-down removed idle VM".into(),
            );
        }
        self.push_event(
            PoolEventType::Scaled,
            "scaled",
            None,
            format!("target_idle set to {}", clamped),
        );
        self.ensure_idle_target();
        clamped
    }

    pub fn recycle(&self, scope: RecycleScope, reason: &str) -> usize {
        let mut recycled = 0usize;
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            if matches!(scope, RecycleScope::Idle | RecycleScope::All) {
                while let Some(vm) = state.idle.pop_front() {
                    recycled += 1;
                    self.metrics.pool_recycles.fetch_add(1, Ordering::Relaxed);
                    *state
                        .recycle_counts
                        .entry(RecycleReason::Manual.as_str().to_string())
                        .or_insert(0) += 1;
                    self.push_event(
                        PoolEventType::Recycled,
                        RecycleReason::Manual.as_str(),
                        Some(vm.id),
                        format!("manual recycle ({})", reason),
                    );
                }
            }
            if matches!(scope, RecycleScope::All) {
                state.recycle_generation += 1;
            }
        }
        self.ensure_idle_target();
        self.condvar.notify_all();
        recycled
    }

    pub fn probe_one_idle(&self, timeout: Duration) {
        let maybe_vm = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.idle.pop_front()
        };
        let Some(mut vm) = maybe_vm else {
            return;
        };

        let probe_result = health::probe_vm(vm.vm.as_mut(), &self.language, timeout);
        vm.last_health_probe = Instant::now();
        match probe_result {
            Ok(()) => {
                self.push_event(
                    PoolEventType::HealthProbePassed,
                    "health_probe",
                    Some(vm.id),
                    "idle VM passed background probe".into(),
                );
                self.finalize_vm(vm, LeaseOutcome::ReturnedIdle);
            }
            Err(err) => {
                self.metrics.health_failures.fetch_add(1, Ordering::Relaxed);
                self.push_event(
                    PoolEventType::HealthProbeFailed,
                    RecycleReason::HealthProbeFailure.as_str(),
                    Some(vm.id),
                    format!("idle probe failed: {}", err),
                );
                self.finalize_vm(
                    vm,
                    LeaseOutcome::Quarantined {
                        reason: RecycleReason::HealthProbeFailure,
                    },
                );
            }
        }
    }

    pub fn snapshot(&self) -> LaneSnapshot {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let avg_borrow_latency_ms = if state.borrow_count == 0 {
            0.0
        } else {
            state.borrow_latency_ms_sum / state.borrow_count as f64
        };
        let events = match self.events.lock() {
            Ok(events) => events,
            Err(poisoned) => poisoned.into_inner(),
        };
        LaneSnapshot {
            idle: state.idle.len(),
            active: state.active,
            target_idle: state.target_idle,
            waiters: state.waiters,
            healthy_idle: state.idle.len(),
            quarantined: state.quarantined_total,
            avg_borrow_latency_ms,
            recent_recycles: recent_recycle_reasons(&events, &self.language, 5),
        }
    }
}

pub struct PoolManager {
    lanes: HashMap<String, Arc<PoolLane>>,
    config: PoolConfig,
    metrics: Arc<Metrics>,
    events: Arc<Mutex<VecDeque<PoolEvent>>>,
    next_vm_id: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
}

impl PoolManager {
    pub fn new(
        languages: Vec<String>,
        config: PoolConfig,
        factory: Arc<dyn VmFactory>,
        metrics: Arc<Metrics>,
    ) -> Result<Arc<Self>> {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let next_vm_id = Arc::new(AtomicU64::new(0));
        let mut lanes = HashMap::new();
        for language in languages {
            check_language_known(&language)?;
            lanes.insert(
                language.clone(),
                Arc::new(PoolLane {
                    language,
                    config: config.clone(),
                    factory: factory.clone(),
                    metrics: metrics.clone(),
                    state: Mutex::new(PoolLaneState {
                        target_idle: config.min_idle_per_lang,
                        ..PoolLaneState::default()
                    }),
                    condvar: Condvar::new(),
                    events: events.clone(),
                    next_vm_id: next_vm_id.clone(),
                }),
            );
        }
        Ok(Arc::new(Self {
            lanes,
            config,
            metrics,
            events,
            next_vm_id,
            running: Arc::new(AtomicBool::new(true)),
        }))
    }

    pub fn bootstrap(self: &Arc<Self>) {
        for lane in self.lanes.values() {
            lane.ensure_idle_target();
        }
    }

    pub fn start_background_tasks(self: &Arc<Self>) {
        let pool = self.clone();
        let interval = Duration::from_secs(self.config.health_check_interval_secs.max(1));
        thread::spawn(move || {
            while pool.running.load(Ordering::Relaxed) {
                thread::sleep(interval);
                for lane in pool.lanes.values() {
                    lane.probe_one_idle(Duration::from_secs(2));
                }
            }
        });
    }

    pub fn borrow(&self, language: &str, timeout: Duration) -> Result<VmLease> {
        let lane = self
            .lanes
            .get(language)
            .ok_or_else(|| anyhow!("no pool lane for {}", language))?;
        lane.borrow(timeout)
    }

    pub fn has_language(&self, language: &str) -> bool {
        self.lanes.contains_key(language)
    }

    pub fn set_targets(&self, targets: &HashMap<String, usize>) -> Result<PoolStatusSnapshot> {
        for (language, target) in targets {
            let lane = self
                .lanes
                .get(language)
                .ok_or_else(|| anyhow!("unknown pool language {}", language))?;
            lane.set_target_idle(*target);
        }
        Ok(self.status())
    }

    pub fn recycle_languages(
        &self,
        languages: &[String],
        scope: RecycleScope,
        reason: &str,
    ) -> Result<HashMap<String, usize>> {
        let mut results = HashMap::new();
        let selected: Vec<String> = if languages.is_empty() {
            self.lanes.keys().cloned().collect()
        } else {
            languages.to_vec()
        };
        for language in selected {
            let lane = self
                .lanes
                .get(&language)
                .ok_or_else(|| anyhow!("unknown pool language {}", language))?;
            results.insert(language, lane.recycle(scope, reason));
        }
        Ok(results)
    }

    pub fn status(&self) -> PoolStatusSnapshot {
        let mut lanes = HashMap::new();
        let mut healthy = true;
        for (language, lane) in &self.lanes {
            let snapshot = lane.snapshot();
            if snapshot.target_idle > 0 && snapshot.idle == 0 && snapshot.active == 0 {
                healthy = false;
            }
            lanes.insert(language.clone(), snapshot);
        }
        pool_status_now(lanes, healthy)
    }

    pub fn events(&self, limit: usize) -> Vec<PoolEvent> {
        let events = match self.events.lock() {
            Ok(events) => events,
            Err(poisoned) => poisoned.into_inner(),
        };
        events
            .iter()
            .rev()
            .take(limit.max(1))
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::GuestResponse;

    struct FakeVm {
        fork_time: f64,
        response: Option<Result<GuestResponse>>,
    }

    impl super::super::types::ManagedVm for FakeVm {
        fn send_serial(&mut self, _data: &[u8]) -> Result<()> {
            Ok(())
        }

        fn run_until_response_timeout(
            &mut self,
            _timeout: Option<Duration>,
        ) -> Result<GuestResponse> {
            self.response.take().unwrap_or_else(|| {
                Ok(GuestResponse {
                    request_id: "x".into(),
                    exit_code: 0,
                    error_type: "ok".into(),
                    stdout: b"ok\n".to_vec(),
                    stderr: Vec::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    recycle_requested: false,
                })
            })
        }

        fn fork_time_us(&self) -> f64 {
            self.fork_time
        }
    }

    struct FakeFactory {
        created: AtomicU64,
    }

    impl FakeFactory {
        fn new() -> Self {
            Self {
                created: AtomicU64::new(0),
            }
        }
    }

    impl VmFactory for FakeFactory {
        fn create(&self, _language: &str) -> Result<Box<dyn super::super::types::ManagedVm>> {
            self.created.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(FakeVm {
                fork_time: 100.0,
                response: None,
            }))
        }
    }

    fn test_pool() -> Arc<PoolManager> {
        let pool = PoolManager::new(
            vec!["python".into()],
            PoolConfig {
                min_idle_per_lang: 1,
                max_idle_per_lang: 2,
                borrow_timeout_ms: 5,
                health_check_interval_secs: 30,
                max_requests_per_vm: 2,
                max_cumulative_exec_ms_per_vm: 1000,
                event_buffer_size: 32,
            },
            Arc::new(FakeFactory::new()),
            Arc::new(Metrics::new()),
        )
        .unwrap();
        pool.bootstrap();
        pool
    }

    #[test]
    fn borrow_from_idle_lane() {
        let pool = test_pool();
        let lease = pool.borrow("python", Duration::from_millis(10)).unwrap();
        assert_eq!(lease.vm_id(), 1);
    }

    #[test]
    fn recycle_idle_scope_drops_idle_vms() {
        let pool = test_pool();
        let recycled = pool
            .recycle_languages(&["python".into()], RecycleScope::Idle, "test")
            .unwrap();
        assert_eq!(recycled.get("python").copied(), Some(1));
    }

    #[test]
    fn second_borrow_times_out_when_lane_is_exhausted() {
        let pool = PoolManager::new(
            vec!["python".into()],
            PoolConfig {
                min_idle_per_lang: 1,
                max_idle_per_lang: 1,
                borrow_timeout_ms: 1,
                health_check_interval_secs: 30,
                max_requests_per_vm: 2,
                max_cumulative_exec_ms_per_vm: 1000,
                event_buffer_size: 32,
            },
            Arc::new(FakeFactory::new()),
            Arc::new(Metrics::new()),
        )
        .unwrap();
        pool.bootstrap();

        let _held = pool.borrow("python", Duration::from_millis(5)).unwrap();
        let err = pool
            .borrow("python", Duration::from_millis(1))
            .err()
            .expect("expected borrow timeout");
        assert!(err.to_string().contains("timed out"));
    }

    #[test]
    fn dropped_lease_is_quarantined() {
        let pool = test_pool();
        {
            let _lease = pool.borrow("python", Duration::from_millis(10)).unwrap();
        }
        let snapshot = pool.status();
        assert_eq!(snapshot.lanes["python"].quarantined, 1);
    }
}
