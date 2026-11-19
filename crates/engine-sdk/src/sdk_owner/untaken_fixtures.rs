//! Executions no owner took, as the tests here and in the crates above drive them.
//! What Alondra does with an input its SDK session did not take, and with work of the node's own
//! that ended with no turn to take up its notice. The clock is a parameter: every pass of the one
//! clock gets the time it runs at.
use crate::sdk_owner::SdkOwners as _;
use crate::live::LiveOf as _;
use super::*;
use alondra_engine_admission::AdmissionOf as _;
use crate::obligation_watch::{Watch, pass};
use alondra_domain_rules::obligation_policy::Millis;
use alondra_domain::{Owed, Party};


pub fn enforcing_every_deadline() -> Watch {
    Watch::enforcing(|owed| matches!(owed, Owed::HandInput | Owed::Wake))
}

pub fn told(slice: &Slice, text: &str) -> usize {
    slice.prompts().iter().filter(|prompt| prompt.contains(text)).count()
}

/// Sends what the store issues and reads the host until Claude got `text` `times` times.
pub fn until_prompted(slice: &Slice, driver: &mut Driver, text: &str, times: usize) {
    while told(slice, text) < times {
        if driver.pending_permits() > 0 {
            driver.step(&|| false).unwrap();
        } else if let Err(error) = driver.observe() {
            panic!("{error} before Claude got {text:?} {times} times: {:?}", slice.prompts());
        }
    }
}

/// The test steps the owner of `driver` itself, as the daemon would.
pub fn drive_by_hand(slice: &Slice, driver: &Driver) {
    slice.domain.sdk_owners().driven_by_hand.lock().unwrap().insert(driver.agent().clone());
}
