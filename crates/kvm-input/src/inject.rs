use std::sync::{Arc, Mutex};

use kvm_core::InputCommand;

use crate::InputError;

/// Applies input commands on the local machine (client side).
pub trait Injector: Send {
    fn inject(&mut self, cmd: InputCommand) -> Result<(), InputError>;
}

/// Discards every command. Useful when running headless purely to exercise the
/// network/protocol path.
#[derive(Default)]
pub struct NoopInjector;

impl Injector for NoopInjector {
    fn inject(&mut self, _cmd: InputCommand) -> Result<(), InputError> {
        Ok(())
    }
}

/// Records every command into a shared log so tests can assert exactly what
/// would have been injected — this is what makes the end-to-end forwarding test
/// possible without real input permissions.
#[derive(Clone, Default)]
pub struct MockInjector {
    log: Arc<Mutex<Vec<InputCommand>>>,
}

impl MockInjector {
    pub fn new() -> Self {
        Self::default()
    }

    /// A handle to the same underlying log, for the asserting side of a test.
    pub fn log(&self) -> Arc<Mutex<Vec<InputCommand>>> {
        Arc::clone(&self.log)
    }

    pub fn recorded(&self) -> Vec<InputCommand> {
        self.log.lock().unwrap().clone()
    }
}

impl Injector for MockInjector {
    fn inject(&mut self, cmd: InputCommand) -> Result<(), InputError> {
        self.log.lock().unwrap().push(cmd);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_records_commands() {
        let mut m = MockInjector::new();
        m.inject(InputCommand::MouseMoveAbs { x: 1, y: 2 }).unwrap();
        m.inject(InputCommand::MouseButton {
            button: 1,
            pressed: true,
        })
        .unwrap();
        assert_eq!(
            m.recorded(),
            vec![
                InputCommand::MouseMoveAbs { x: 1, y: 2 },
                InputCommand::MouseButton {
                    button: 1,
                    pressed: true
                },
            ]
        );
    }
}
