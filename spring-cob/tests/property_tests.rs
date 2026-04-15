//! Property-based tests for COB parser and VM robustness.

use proptest::prelude::*;
use spring_cob::{CobVm, parse_cob};

proptest! {
    /// Random bytes should not panic the COB parser.
    #[test]
    fn cob_parser_doesnt_panic_on_garbage(data in proptest::collection::vec(any::<u8>(), 0..500)) {
        let _ = parse_cob(&data);
    }

    /// Truncated valid-looking headers should not panic.
    #[test]
    fn cob_parser_doesnt_panic_on_truncated_header(len in 0usize..100) {
        let mut data = vec![0u8; 100];
        // Set version to 4 (valid).
        data[0..4].copy_from_slice(&4i32.to_le_bytes());
        // Set NumberOfScripts to 1.
        data[4..8].copy_from_slice(&1i32.to_le_bytes());
        let _ = parse_cob(&data[..len.min(data.len())]);
    }

    /// A valid COB file with random ticks should not panic the VM.
    #[test]
    fn vm_doesnt_panic_on_random_ticks(
        tick_count in 1usize..50,
        dt in 0i32..1000,
    ) {
        // Use bit.cob if available.
        let scripts_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = scripts_dir.parent().unwrap_or(&scripts_dir);
        let cob_path = workspace_root.join("upstream/Kernel-Panic/scripts/bit.cob");
        if !cob_path.exists() {
            return Ok(());
        }

        let data = std::fs::read(&cob_path).unwrap();
        let cob = parse_cob(&data).unwrap();
        let mut vm = CobVm::new(&cob);

        vm.start_script(&cob, "Create", &[]);

        for _ in 0..tick_count {
            let _ = vm.tick(&cob, dt);
        }
    }
}
