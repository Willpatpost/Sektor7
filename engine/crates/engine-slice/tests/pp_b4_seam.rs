//! PP-B4 checkpoint 1's portability record: the trait bounds the
//! future I/O worker will require, asserted at compile time so they
//! cannot drift silently. The worker thread will own the campaign's
//! one `SessionStore` connection exclusively (`Send`, not `Sync` —
//! nothing shares it; coordination happens through channels), and
//! the job payloads carry the engine-persist value types across the
//! thread boundary (`Send`). The crossing and its T1 journal stay
//! on the authority thread (the sidecar-lock discipline), so no
//! `Send` requirement is asserted for them.

#[test]
fn the_worker_ownership_types_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<engine_slice::store::SessionStore>();
    assert_send::<engine_slice::store::StoreLocation>();
    assert_send::<engine_persist::RevisionRecord>();
    assert_send::<engine_persist::StoredObject>();
    assert_send::<engine_persist::AppendOutcome>();
    assert_send::<engine_persist::Reconciliation>();
    assert_send::<engine_persist::ChainSummary>();
}
