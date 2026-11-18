//! Claude's sign-in listeners on a test's Mac: each return handed back is recorded, and answered
//! with the status the test chose (200 unless it chose another, or none for a listener gone). A
//! listener listens until a return is handed back to it, as Claude's closes once it has the code.
use alondra_engine_ports::Registry;
use alondra_engine_ports::sign_in_returns::SignInReturns as Port;
use std::sync::{Arc, Mutex, PoisonError};

pub struct SignInReturns {
    handed: Mutex<Vec<(u16, String)>>,
    answer: Mutex<Result<u16, String>>,
}

impl Default for SignInReturns {
    fn default() -> Self {
        Self { handed: Mutex::default(), answer: Mutex::new(Ok(200)) }
    }
}

pub fn wire(registry: &Registry) -> Arc<SignInReturns> {
    let returns = Arc::new(SignInReturns::default());
    registry.provide::<dyn Port>(returns.clone());
    registry.provide::<SignInReturns>(returns.clone());
    returns
}

/// The doubled listeners `registry` was given.
pub fn of(registry: &Registry) -> Arc<SignInReturns> {
    registry.port::<SignInReturns>().expect("the test's runtime has the doubled sign-in listeners")
}

impl SignInReturns {
    /// The returns handed back so far: the listener's port and what was asked of it.
    pub fn handed(&self) -> Vec<(u16, String)> {
        self.handed.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// What every listener answers from now on.
    pub fn answer(&self, answer: Result<u16, String>) {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = answer;
    }
}

impl Port for SignInReturns {
    fn hand_back(&self, port: u16, target: &str) -> Result<u16, String> {
        self.handed.lock().unwrap_or_else(PoisonError::into_inner).push((port, target.to_owned()));
        self.answer.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    fn listening(&self, port: u16) -> bool {
        !self.handed.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(handed, _)| *handed == port)
    }
}
