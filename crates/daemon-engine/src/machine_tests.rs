use super::*;
use crate::test_support::Directory;
use alondra_core::accounts::AccountId;
use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};

const AT: alondra_core::Millis = 1_790_192_202_000;

#[tokio::test]
async fn a_sample_takes_the_accounts_again_from_what_the_domain_holds_now() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(crate::wiring::open(&home).unwrap());
    let gauges: Gauges = Default::default();
    *gauges.lock().unwrap() = Some((alondra_core::Machine { at: AT, ..Default::default() }, None, vec![]));

    let weekly = WindowReading { kind: WindowKind::Weekly, used: Share::of(0.1), resets_at: None, exhausted: false };
    crate::usage_probe::record(&*domain, &AccountId::default_account(), UsageReading { windows: vec![weekly], plan: None, complete: true }, AT);
    assert!(gauges.lock().unwrap().as_ref().unwrap().2.is_empty(), "the sample took the accounts before the probe committed");

    resample(&gauges, &*domain);

    let sample = gauges.lock().unwrap();
    let (machine, _, providers) = sample.as_ref().unwrap();
    assert_eq!(machine.at, AT, "the sample keeps its instant");
    assert_eq!(providers[0].accounts[0].measured_at, Some(AT));
}

#[tokio::test]
async fn resampling_before_any_sample_leaves_the_gauges_empty() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(crate::wiring::open(&home).unwrap());
    let gauges: Gauges = Default::default();
    resample(&gauges, &*domain);
    assert!(gauges.lock().unwrap().is_none());
}
