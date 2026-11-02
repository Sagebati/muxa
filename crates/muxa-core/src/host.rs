//! What the process may use on the machine it runs on, for sizing defaults.
//!
//! Plugins derive their unset tunables (pool sizes, …) from here, so the same
//! binary is sized for a 1-CPU container and a 32-core host without any
//! configuration. An explicit value in the app's configuration always wins.

use std::sync::OnceLock;
use std::thread;

/// CPUs this process may run on: [`std::thread::available_parallelism`], which
/// honours the cgroup CPU quota (a container's `--cpus` / Kubernetes CPU
/// limit) and the scheduler affinity mask, not just the host's core count.
/// `1` if it can't be determined. Read once, then cached.
#[must_use]
pub fn cpus() -> usize {
    static CPUS: OnceLock<usize> = OnceLock::new();
    *CPUS.get_or_init(|| thread::available_parallelism().map_or(1, usize::from))
}

/// Default size for a pool of connections to a networked database server
/// (Postgres, MySQL): four per CPU, between 4 and 32.
///
/// Four per CPU (deadpool's own heuristic) keeps every CPU busy while its
/// queries are in flight on the server. The floor leaves headroom for
/// background tasks next to request handlers on a 1-CPU container. The cap
/// matters because every connection is a server process (Postgres allows 100
/// by default), and replicas multiply it.
#[must_use]
pub fn db_pool_size() -> u32 {
    db_pool_size_for(cpus())
}

fn db_pool_size_for(cpus: usize) -> u32 {
    let per_cpu = u32::try_from(cpus).unwrap_or(u32::MAX).saturating_mul(4);
    per_cpu.clamp(4, 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpus_is_at_least_one_and_stable() {
        assert!(cpus() >= 1);
        assert_eq!(cpus(), cpus());
    }

    #[test]
    fn db_pool_size_scales_with_cpus_within_bounds() {
        assert_eq!(db_pool_size_for(0), 4);
        assert_eq!(db_pool_size_for(1), 4);
        assert_eq!(db_pool_size_for(2), 8);
        assert_eq!(db_pool_size_for(4), 16);
        assert_eq!(db_pool_size_for(8), 32);
        assert_eq!(db_pool_size_for(64), 32);
        assert_eq!(db_pool_size_for(usize::MAX), 32);
        assert_eq!(db_pool_size(), db_pool_size_for(cpus()));
    }
}
