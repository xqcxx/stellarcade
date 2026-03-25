#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env, symbol_short};

struct Setup<'a> {
    env: Env,
    client: SettlementQueueClient<'a>,
    admin: Address,
    reward: Address,
    treasury: Address,
}

fn setup() -> Setup<'static> {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(SettlementQueue, ());
    let client = SettlementQueueClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let reward = Address::generate(&env);
    let treasury = Address::generate(&env);

    client.init(&admin, &reward, &treasury);

    // Safety: we are using a static lifetime for the client because the env won't go out of scope in the test
    let client: SettlementQueueClient<'static> = unsafe { core::mem::transmute(client) };

    Setup {
        env,
        client,
        admin,
        reward,
        treasury,
    }
}

#[test]
fn test_init() {
    let _s = setup();
}

#[test]
fn test_enqueue_and_process() {
    let s = setup();
    let user = Address::generate(&s.env);
    let s_id = symbol_short!("s1");

    s.client.enqueue_settlement(&s_id, &user, &1000i128, &symbol_short!("win"));

    let state = s.client.settlement_state(&s_id).unwrap();
    assert_eq!(state.status, SettlementStatus::Pending);
    assert_eq!(state.amount, 1000);

    s.client.process_next(&1);

    let state = s.client.settlement_state(&s_id).unwrap();
    assert_eq!(state.status, SettlementStatus::Processed);
}

#[test]
fn test_fifo_processing() {
    let s = setup();
    let user = Address::generate(&s.env);
    
    let s1 = symbol_short!("s1");
    let s2 = symbol_short!("s2");

    s.client.enqueue_settlement(&s1, &user, &100, &symbol_short!("r1"));
    s.client.enqueue_settlement(&s2, &user, &200, &symbol_short!("r2"));

    s.client.process_next(&1);
    
    assert_eq!(s.client.settlement_state(&s1).unwrap().status, SettlementStatus::Processed);
    assert_eq!(s.client.settlement_state(&s2).unwrap().status, SettlementStatus::Pending);

    s.client.process_next(&1);
    assert_eq!(s.client.settlement_state(&s2).unwrap().status, SettlementStatus::Processed);
}

#[test]
fn test_mark_failed() {
    let s = setup();
    let user = Address::generate(&s.env);
    let s_id = symbol_short!("s1");

    s.client.enqueue_settlement(&s_id, &user, &500, &symbol_short!("fail"));
    s.client.mark_failed(&s_id, &404);

    let state = s.client.settlement_state(&s_id).unwrap();
    assert_eq!(state.status, SettlementStatus::Failed);
    assert_eq!(state.error_code, Some(404));
}

#[test]
fn test_get_batch_status_empty() {
    let s = setup();
    let status = s.client.get_batch_status(&0, &10);
    assert_eq!(status.pending, 0);
    assert_eq!(status.processing, 0);
    assert_eq!(status.succeeded, 0);
    assert_eq!(status.failed, 0);
}

#[test]
fn test_get_batch_status_mixed() {
    let s = setup();
    let user = Address::generate(&s.env);
    
    // Total 5 items
    let s0 = symbol_short!("s0");
    let s1 = symbol_short!("s1");
    let s2 = symbol_short!("s2");
    let s3 = symbol_short!("s3");
    let s4 = symbol_short!("s4");
    
    s.client.enqueue_settlement(&s0, &user, &100, &symbol_short!("test"));
    s.client.enqueue_settlement(&s1, &user, &100, &symbol_short!("test"));
    s.client.enqueue_settlement(&s2, &user, &100, &symbol_short!("test"));
    s.client.enqueue_settlement(&s3, &user, &100, &symbol_short!("test"));
    s.client.enqueue_settlement(&s4, &user, &100, &symbol_short!("test"));

    // Process 2: index 0, 1 -> Processed
    s.client.process_next(&2);

    // Mark 1 as failed: index 2 -> Failed
    s.client.mark_failed(&symbol_short!("s2"), &500);

    // Remaining indices 3, 4 are Pending.

    let status = s.client.get_batch_status(&0, &4);
    assert_eq!(status.succeeded, 2); // 0, 1
    assert_eq!(status.failed, 1);    // 2
    assert_eq!(status.pending, 2);   // 3, 4
    assert_eq!(status.processing, 0);
}

#[test]
fn test_get_batch_status_invalid_range() {
    let s = setup();
    // Start > End should probably error or return zero counts
    // In our implementation we'll likely return zero counts or error.
    // The requirement says "Accept batch id or range parameters with validation bounds."
    // So we should probably return an Error if the range is invalid.
    
    let result = s.client.try_get_batch_status(&10, &5);
    assert!(result.is_err());
}
