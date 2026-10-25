use super::{Gate, PrincipalQuotas};
use uuid::Uuid;
fn options() -> PrincipalQuotas {
    PrincipalQuotas {
        requests_per_minute: 2,
        max_inflight: 1,
        max_subscriptions: 1,
        max_identities: 2,
    }
}
#[test]
fn a_principal_cannot_starve_another_or_bypass_the_rate_budget_by_releasing_slots() {
    let gate = Gate::new(options());
    let scope = Uuid::now_v7();
    let alice = (scope, Uuid::now_v7());
    let bob = (scope, Uuid::now_v7());
    let a = gate.acquire(alice, false).unwrap();
    assert!(gate.acquire(alice, false).is_none());
    let b = gate.acquire(bob, false).unwrap();
    drop(a);
    let a = gate.acquire(alice, false).unwrap();
    drop(a);
    assert!(gate.acquire(alice, false).is_none());
    drop(b);
    assert!(gate.acquire(bob, false).is_some());
}
#[test]
fn subscription_slots_are_owned_until_drop_and_identity_maps_are_bounded() {
    let gate = Gate::new(options());
    let scope = Uuid::now_v7();
    let alice = (scope, Uuid::now_v7());
    let bob = (scope, Uuid::now_v7());
    let stream = gate.acquire(alice, true).unwrap();
    assert!(gate.acquire(alice, true).is_none());
    let other = gate.acquire(bob, true).unwrap();
    assert!(gate.acquire((scope, Uuid::now_v7()), false).is_none());
    drop(stream);
    assert!(gate.acquire(alice, true).is_some());
    drop(other);
}
