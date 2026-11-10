use alondra_core::{Millis, accounts::AccountId};

/// The credential of the bindings that run on the provider's own sign-in on this Mac.
const NATIVE_LOGIN: &str = "native-login";

/// The account a provider binding's executions run on: its credential names it.
pub fn account_of(credential: &str) -> AccountId {
    match credential {
        NATIVE_LOGIN => AccountId::default_account(),
        other => AccountId::parse(other).unwrap_or_else(|_| AccountId::default_account()),
    }
}

pub fn local_day(millis: Millis) -> String {
    let t = millis.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> AccountId {
        AccountId::parse(text).unwrap()
    }

#[test]
fn a_binding_on_the_native_sign_in_runs_on_the_default_account() {
    assert!(account_of("native-login").is_default());
    assert_eq!(account_of("work"), id("work"));
    assert!(account_of("Not An Id").is_default());
}

}
