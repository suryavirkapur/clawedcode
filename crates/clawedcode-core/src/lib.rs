pub mod background_task;
pub mod compat;
pub mod config;
pub mod content;
pub mod interactive;
pub mod onboarding;
pub mod permissions;
pub mod prompt;
pub mod runtime;
pub mod session;
pub mod subagent;
pub mod tasks;
pub mod tool_input;
pub mod update;

#[cfg(test)]
pub mod test_support {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    pub fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        match LOCK.get_or_init(|| Mutex::new(())).lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}
