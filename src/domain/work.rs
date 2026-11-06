//! Work budget (docs `work-budget.md` §4): whether this node can finish an
//! announced sub-task before both its own timeout and the orchestrator's
//! deadline, given everything it already holds or has bid on.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::domain::p2p::Work;

/// Announced budgets are kept as long as the orchestrator keeps a bid pool.
const ANNOUNCED_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// A bid counts toward the queue until its dispatch arrives or this passes.
/// The orchestrator collects bids for at most 5 s.
const RESERVATION_TTL: Duration = Duration::from_secs(15);

/// Bounds a measured rate must fall in: 1 MB/s to 10 TB/s, 1 Mflop/s to
/// 100 Tflop/s. Anything else is a failed measurement.
const SECONDS_PER_BYTE: (f64, f64) = (1e-13, 1e-6);
const SECONDS_PER_FLOP: (f64, f64) = (1e-14, 1e-6);

/// Seconds per byte and per flop of the connected core, from `/sysinfo`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TnRates {
    pub seconds_per_byte: f64,
    pub seconds_per_flop: f64,
}

impl TnRates {
    /// `None` when core does not report both rates (an older core, or a
    /// failed measurement) or either is out of bounds.
    pub fn from_sysinfo(per_byte: Option<f64>, per_flop: Option<f64>) -> Option<Self> {
        let within = |x: f64, (lo, hi): (f64, f64)| x.is_finite() && x >= lo && x <= hi;
        match (per_byte, per_flop) {
            (Some(b), Some(f)) if within(b, SECONDS_PER_BYTE) && within(f, SECONDS_PER_FLOP) => {
                Some(Self {
                    seconds_per_byte: b,
                    seconds_per_flop: f,
                })
            }
            _ => None,
        }
    }

    pub fn seconds(&self, work: &Work) -> f64 {
        work.traffic_bytes as f64 * self.seconds_per_byte
            + work.flops as f64 * self.seconds_per_flop
    }
}

/// What the node accepted for one parent task.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    /// Seconds from dispatch the orchestrator allows.
    pub deadline_secs: u32,
    /// This sub-task alone, without the queue ahead of it.
    pub expected_secs: f64,
}

/// How long the node may wait for core on a sub-task it accepted `waited`
/// ago: its own timeout, or what is left of the orchestrator's deadline when
/// that is shorter. `None` when the deadline has already passed.
pub fn compute_timeout(
    own_secs: u64,
    budget: Option<&Budget>,
    waited: Duration,
) -> Option<Duration> {
    let own = Duration::from_secs(own_secs);
    match budget {
        None => Some(own),
        Some(b) => {
            let left = Duration::from_secs(u64::from(b.deadline_secs)).checked_sub(waited)?;
            (!left.is_zero()).then(|| own.min(left))
        }
    }
}

/// Settings for [`WorkLedger::decide`].
#[derive(Debug, Clone, Copy)]
pub struct BudgetPolicy {
    pub rates: TnRates,
    /// Proving and transfer, added to every estimate.
    pub overhead_secs: f64,
    /// Share of the limit the estimate may use.
    pub margin: f64,
    /// `WQC_COMPUTE_TIMEOUT_SECS`.
    pub own_timeout_secs: u64,
    /// What a held sub-task without work counts for (`WQC_WORK_UNKNOWN_SECS`).
    pub unknown_secs: f64,
}

struct Held {
    /// `None` for a sub-task without work.
    expected_secs: Option<f64>,
    started: Option<Instant>,
}

/// Budgets this node bid on, by parent task; bids not yet dispatched; and
/// every sub-task it holds.
#[derive(Default)]
pub struct WorkLedger {
    announced: HashMap<String, (Budget, Instant)>,
    reserved: HashMap<String, (f64, Instant)>,
    held: HashMap<String, Held>,
}

impl WorkLedger {
    /// Expected time before a new sub-task would start: every held sub-task
    /// (one without work counts `unknown_secs`; one past its estimate counts
    /// the whole estimate again), plus bids still waiting for a dispatch.
    pub fn queued_secs(&self, now: Instant, unknown_secs: f64) -> f64 {
        let held: f64 = self
            .held
            .values()
            .map(|h| match (h.expected_secs, h.started) {
                (None, _) => unknown_secs,
                (Some(expected), None) => expected,
                (Some(expected), Some(at)) => {
                    let left = expected - now.duration_since(at).as_secs_f64();
                    if left > 0.0 {
                        left
                    } else {
                        expected
                    }
                }
            })
            .sum();
        let reserved: f64 = self
            .reserved
            .values()
            .filter(|(_, until)| now < *until)
            .map(|(expected, _)| expected)
            .sum();
        held + reserved
    }

    /// Records the budget and a reservation, and returns the budget, when the
    /// queue plus this work fits `margin · min(deadline, own timeout)`;
    /// otherwise returns the expected total.
    pub fn decide(
        &mut self,
        task_id: &str,
        work: &Work,
        deadline_secs: u32,
        policy: &BudgetPolicy,
        now: Instant,
    ) -> Result<Budget, f64> {
        self.reserved.retain(|_, (_, until)| now < *until);
        let own = policy.rates.seconds(work) + policy.overhead_secs;
        let total = self.queued_secs(now, policy.unknown_secs) + own;
        let limit = policy.margin * policy.own_timeout_secs.min(u64::from(deadline_secs)) as f64;
        if total > limit {
            return Err(total);
        }
        self.announced
            .retain(|_, (_, at)| now.duration_since(*at) < ANNOUNCED_TTL);
        let budget = Budget {
            deadline_secs,
            expected_secs: own,
        };
        self.announced.insert(task_id.to_string(), (budget, now));
        self.reserved
            .insert(task_id.to_string(), (own, now + RESERVATION_TTL));
        Ok(budget)
    }

    /// The budget this node bid with on `parent_task_id`, if any.
    pub fn budget(&self, parent_task_id: &str) -> Option<Budget> {
        self.announced.get(parent_task_id).map(|(b, _)| *b)
    }

    /// Counts a sub-task the node accepted (or restored after a restart). Its
    /// parent's reservation, if any, ends here.
    pub fn hold(
        &mut self,
        sub_task_id: &str,
        parent_task_id: Option<&str>,
        budget: Option<&Budget>,
    ) {
        if let Some(parent) = parent_task_id {
            self.reserved.remove(parent);
        }
        self.held.insert(
            sub_task_id.to_string(),
            Held {
                expected_secs: budget.map(|b| b.expected_secs),
                started: None,
            },
        );
    }

    pub fn start(&mut self, sub_task_id: &str, now: Instant) {
        if let Some(h) = self.held.get_mut(sub_task_id) {
            h.started = Some(now);
        }
    }

    pub fn release(&mut self, sub_task_id: &str) {
        self.held.remove(sub_task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> BudgetPolicy {
        BudgetPolicy {
            // 1 GB/s and 1 Gflop/s.
            rates: TnRates {
                seconds_per_byte: 1e-9,
                seconds_per_flop: 1e-9,
            },
            overhead_secs: 5.0,
            margin: 0.5,
            own_timeout_secs: 300,
            unknown_secs: 30.0,
        }
    }

    fn work(gb: u64) -> Work {
        Work {
            traffic_bytes: gb * 1_000_000_000,
            flops: 0,
        }
    }

    #[test]
    fn bids_only_within_half_the_shorter_limit() {
        let now = Instant::now();
        let mut ledger = WorkLedger::default();
        // 100 + 5 s ≤ 0.5 · min(330, 300) = 150.
        let budget = ledger.decide("a", &work(100), 330, &policy(), now).unwrap();
        assert_eq!(
            budget,
            Budget {
                deadline_secs: 330,
                expected_secs: 105.0
            }
        );
        assert_eq!(ledger.budget("a"), Some(budget));
        // The reservation for "a" leaves 45 s: 45 + 5 is past it, 40 + 5 not.
        let later = now + Duration::from_secs(1);
        assert_eq!(
            ledger.decide("b", &work(45), 330, &policy(), later),
            Err(155.0)
        );
        assert!(ledger.decide("c", &work(40), 330, &policy(), later).is_ok());
        // A shorter deadline is the limit: 0.5 · 120 = 60.
        let fresh = &mut WorkLedger::default();
        assert!(fresh.decide("d", &work(60), 120, &policy(), now).is_err());
    }

    #[test]
    fn a_reservation_lapses_without_a_dispatch() {
        let now = Instant::now();
        let mut ledger = WorkLedger::default();
        ledger.decide("a", &work(100), 330, &policy(), now).unwrap();
        assert_eq!(ledger.queued_secs(now, 30.0), 105.0);
        assert_eq!(ledger.queued_secs(now + RESERVATION_TTL, 30.0), 0.0);
    }

    #[test]
    fn held_work_counts_until_it_is_done() {
        let now = Instant::now();
        let mut ledger = WorkLedger::default();
        let budget = ledger.decide("a", &work(100), 330, &policy(), now).unwrap();
        ledger.hold("a_1", Some("a"), Some(&budget));
        // The dispatch ends the reservation, so 105 s are counted once.
        assert_eq!(ledger.queued_secs(now, 30.0), 105.0);
        // 60 s into it, 45 s are left.
        ledger.start("a_1", now);
        assert_eq!(
            ledger.queued_secs(now + Duration::from_secs(60), 30.0),
            45.0
        );
        // Past its estimate it counts in full again.
        assert_eq!(
            ledger.queued_secs(now + Duration::from_secs(200), 30.0),
            105.0
        );
        ledger.release("a_1");
        assert_eq!(ledger.queued_secs(now, 30.0), 0.0);
    }

    #[test]
    fn work_without_a_budget_counts_the_unknown_share() {
        let now = Instant::now();
        let mut ledger = WorkLedger::default();
        ledger.hold("mps_1", Some("mps"), None);
        ledger.hold("restored", None, None);
        assert_eq!(ledger.queued_secs(now, 30.0), 60.0);
        // 60 + 85 + 5 = 150 fits; one more second does not.
        assert!(ledger.decide("a", &work(85), 330, &policy(), now).is_ok());
    }

    #[test]
    fn waits_for_core_until_what_is_left_of_the_deadline() {
        let budget = Budget {
            deadline_secs: 120,
            expected_secs: 1.0,
        };
        let zero = Duration::ZERO;
        assert_eq!(
            compute_timeout(300, Some(&budget), zero),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            compute_timeout(60, Some(&budget), zero),
            Some(Duration::from_secs(60))
        );
        // 100 s in the queue leave 20 s.
        assert_eq!(
            compute_timeout(300, Some(&budget), Duration::from_secs(100)),
            Some(Duration::from_secs(20))
        );
        // The deadline has passed.
        assert_eq!(
            compute_timeout(300, Some(&budget), Duration::from_secs(120)),
            None
        );
        assert_eq!(
            compute_timeout(300, None, Duration::from_secs(1000)),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn rates_need_both_fields_within_bounds() {
        assert_eq!(TnRates::from_sysinfo(None, Some(1e-9)), None);
        assert_eq!(TnRates::from_sysinfo(Some(0.0), Some(1e-9)), None);
        // A zero flop rate is a failed measurement.
        assert_eq!(TnRates::from_sysinfo(Some(1e-10), Some(0.0)), None);
        assert_eq!(
            TnRates::from_sysinfo(Some(1e-10), Some(f64::INFINITY)),
            None
        );
        assert!(TnRates::from_sysinfo(Some(5.7e-11), Some(3.3e-10)).is_some());
    }
}
