//! Small machine probes for the scheduler's guards (load, free memory, disk, clock). All read-only; the per-OS work lives in `platform`.

pub use crate::platform::{date_str, disk_free_gb, free_gb, load1, local_hour, ncpu, parse_df, parse_vm_stat};

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: probes return sane values on this machine.
    #[test]
    fn probes_return_sane_values_on_this_machine() {
        assert!(ncpu() >= 1);
        assert!(load1().is_none_or(|l| l >= 0.0));
        assert!(free_gb().is_none_or(|g| g > 0.0));
        assert!(date_str(1_791_916_200).len() == 10);
    }
}
