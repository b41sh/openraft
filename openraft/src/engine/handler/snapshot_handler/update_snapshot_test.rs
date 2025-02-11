use maplit::btreeset;
use pretty_assertions::assert_eq;

use crate::engine::testing::UTCfg;
use crate::engine::CEngine;
use crate::engine::Engine;
use crate::testing::log_id;
use crate::Membership;
use crate::MetricsChangeFlags;
use crate::SnapshotMeta;
use crate::StoredMembership;

fn m12() -> Membership<u64, ()> {
    Membership::<u64, ()>::new(vec![btreeset! {1,2}], None)
}

fn m1234() -> Membership<u64, ()> {
    Membership::<u64, ()>::new(vec![btreeset! {1,2,3,4}], None)
}

fn eng() -> CEngine<UTCfg> {
    let mut eng = Engine::default();
    eng.state.enable_validate = false; // Disable validation for incomplete state

    eng.state.snapshot_meta = SnapshotMeta {
        last_log_id: Some(log_id(2, 2)),
        last_membership: StoredMembership::new(Some(log_id(1, 1)), m12()),
        snapshot_id: "1-2-3-4".to_string(),
    };
    eng
}

#[test]
fn test_update_snapshot_no_update() -> anyhow::Result<()> {
    // snapshot will not be updated because of equal or less `last_log_id`.
    let mut eng = eng();

    let got = eng.snapshot_handler().update_snapshot(SnapshotMeta {
        last_log_id: Some(log_id(2, 2)),
        last_membership: StoredMembership::new(Some(log_id(1, 1)), m1234()),
        snapshot_id: "1-2-3-4".to_string(),
    });

    assert_eq!(false, got);

    assert_eq!(
        SnapshotMeta {
            last_log_id: Some(log_id(2, 2)),
            last_membership: StoredMembership::new(Some(log_id(1, 1)), m12()),
            snapshot_id: "1-2-3-4".to_string(),
        },
        eng.state.snapshot_meta
    );

    assert_eq!(
        MetricsChangeFlags {
            replication: false,
            local_data: false,
            cluster: false,
        },
        eng.output.metrics_flags
    );

    assert_eq!(0, eng.output.take_commands().len());

    Ok(())
}

#[test]
fn test_update_snapshot_updated() -> anyhow::Result<()> {
    // snapshot will be updated to a new one with greater `last_log_id`.
    let mut eng = eng();

    let got = eng.snapshot_handler().update_snapshot(SnapshotMeta {
        last_log_id: Some(log_id(2, 3)),
        last_membership: StoredMembership::new(Some(log_id(2, 2)), m1234()),
        snapshot_id: "1-2-3-4".to_string(),
    });

    assert_eq!(true, got);

    assert_eq!(
        SnapshotMeta {
            last_log_id: Some(log_id(2, 3)),
            last_membership: StoredMembership::new(Some(log_id(2, 2)), m1234()),
            snapshot_id: "1-2-3-4".to_string(),
        },
        eng.state.snapshot_meta
    );

    assert_eq!(
        MetricsChangeFlags {
            replication: false,
            local_data: true,
            cluster: false,
        },
        eng.output.metrics_flags
    );

    assert_eq!(0, eng.output.take_commands().len());

    Ok(())
}
