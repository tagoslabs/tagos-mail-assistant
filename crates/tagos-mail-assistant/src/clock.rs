use std::sync::Mutex;

use chrono::{DateTime, Utc};

/// Time source. Tests and the demo pass a fixed clock; production passes [`SystemClock`].
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// ```
/// use chrono::{TimeZone, Utc};
/// use tagos_mail_assistant::clock::{Clock, FixedClock};
/// let clock = FixedClock::new(Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).single().unwrap());
/// assert_eq!(
///     clock.now(),
///     Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).single().unwrap()
/// );
/// clock.advance_hours(96);
/// assert_eq!(
///     clock.now(),
///     Utc.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).single().unwrap()
/// );
/// ```
#[derive(Debug)]
pub struct FixedClock {
    now: Mutex<DateTime<Utc>>,
}

impl FixedClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    pub fn advance_hours(&self, hours: i64) {
        let mut guard = self.now.lock().expect("clock lock");
        *guard += chrono::Duration::hours(hours);
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.lock().expect("clock lock") = now;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("clock lock")
    }
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
