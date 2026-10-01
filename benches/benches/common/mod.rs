//! Criterion settings shared by the benchmark targets.

use criterion::Criterion;
use pitcrew_benches::Mode;
use std::time::Duration;

/// Criterion configured for [`Mode::from_env`]: few short samples in quick mode, more in full.
/// `criterion_group!` applies the command-line arguments on top.
pub fn criterion() -> Criterion {
    let base = Criterion::default().without_plots();
    match Mode::from_env() {
        Mode::Quick => base
            .sample_size(10)
            .nresamples(10_000)
            .warm_up_time(Duration::from_millis(300))
            .measurement_time(Duration::from_secs(1)),
        Mode::Full => base
            .sample_size(30)
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(3)),
    }
}
