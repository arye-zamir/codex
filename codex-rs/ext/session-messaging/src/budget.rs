use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;

/// Maximum number of outbound session messages between explicit user inputs.
pub(crate) const OUTBOUND_MESSAGE_LIMIT: usize = 4;

/// Bounds how many messages one session may send before the user types again,
/// so a runaway agent cannot flood its peers.
#[derive(Debug, Default)]
pub(crate) struct SessionMessageBudget {
    state: Mutex<BudgetState>,
}

impl SessionMessageBudget {
    /// Reserves one message slot, or returns `None` once the budget is spent.
    /// A reservation dropped without [`SessionMessageReservation::commit`] is
    /// refunded.
    pub(crate) fn reserve(self: &Arc<Self>) -> Option<SessionMessageReservation> {
        let mut state = self.state();
        if state.reservations >= OUTBOUND_MESSAGE_LIMIT {
            return None;
        }
        state.reservations += 1;
        Some(SessionMessageReservation {
            budget: Arc::clone(self),
            epoch: state.epoch,
            committed: false,
        })
    }

    /// Starts a fresh budget; reservations from the previous epoch no longer
    /// refund into it.
    pub(crate) fn reset(&self) {
        let mut state = self.state();
        state.epoch = state.epoch.wrapping_add(1);
        state.reservations = 0;
    }

    fn refund(&self, epoch: u64) {
        let mut state = self.state();
        if state.epoch == epoch {
            state.reservations = state.reservations.saturating_sub(1);
        }
    }

    fn state(&self) -> MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, Default)]
struct BudgetState {
    epoch: u64,
    reservations: usize,
}

pub(crate) struct SessionMessageReservation {
    budget: Arc<SessionMessageBudget>,
    epoch: u64,
    committed: bool,
}

impl SessionMessageReservation {
    /// Consumes the reservation once the message has been accepted for delivery.
    pub(crate) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SessionMessageReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.budget.refund(self.epoch);
        }
    }
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
