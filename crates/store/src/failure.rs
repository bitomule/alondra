use crate::Error;
use std::{cell::Cell, rc::Rc};

#[derive(Default)]
pub(super) struct Failure(Rc<Cell<bool>>);

pub(super) struct Operation {
    failed: Rc<Cell<bool>>,
    armed: bool,
}
impl Failure {
    pub(super) fn begin(&self) -> Result<Operation, Error> {
        if self.0.get() {
            return Err(Error::Poisoned);
        }
        Ok(Operation {
            failed: self.0.clone(),
            armed: true,
        })
    }
}
impl Operation {
    pub(super) fn finish<T, E>(mut self, result: Result<T, E>) -> Result<T, E>
    where
        E: From<Error>,
    {
        match result {
            Ok(_) if self.failed.get() => Err(Error::Poisoned.into()),
            Ok(value) => {
                self.armed = false;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        if self.armed {
            self.failed.set(true);
        }
    }
}
