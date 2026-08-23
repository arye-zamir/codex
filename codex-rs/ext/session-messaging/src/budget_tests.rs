use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;

use super::OUTBOUND_MESSAGE_LIMIT;
use super::SessionMessageBudget;

fn exhaust(budget: &Arc<SessionMessageBudget>) {
    for _ in 0..OUTBOUND_MESSAGE_LIMIT {
        budget.reserve().expect("reservation").commit();
    }
}

#[test]
fn committed_reservations_exhaust_the_budget() {
    let budget = Arc::new(SessionMessageBudget::default());
    exhaust(&budget);
    assert!(budget.reserve().is_none());
}

#[test]
fn uncommitted_reservation_is_refunded() {
    let budget = Arc::new(SessionMessageBudget::default());
    drop(budget.reserve().expect("reservation"));
    exhaust(&budget);
    assert!(budget.reserve().is_none());
}

#[test]
fn reset_starts_a_new_epoch_without_stale_refunds() {
    let budget = Arc::new(SessionMessageBudget::default());
    let stale_reservation = budget.reserve().expect("reservation");
    budget.reset();
    exhaust(&budget);
    drop(stale_reservation);
    assert!(budget.reserve().is_none());
    budget.reset();
    assert!(budget.reserve().is_some());
}

#[test]
fn concurrent_reservations_never_exceed_the_limit() {
    let budget = Arc::new(SessionMessageBudget::default());
    let accepted = Arc::new(AtomicUsize::new(0));
    let workers = (0..32)
        .map(|_| {
            let budget = Arc::clone(&budget);
            let accepted = Arc::clone(&accepted);
            thread::spawn(move || {
                if let Some(reservation) = budget.reserve() {
                    reservation.commit();
                    accepted.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().expect("worker");
    }
    assert_eq!(accepted.load(Ordering::SeqCst), OUTBOUND_MESSAGE_LIMIT);
    assert!(budget.reserve().is_none());
}
