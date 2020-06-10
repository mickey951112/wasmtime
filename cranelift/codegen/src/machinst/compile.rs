//! Compilation backend pipeline: optimized IR to VCode / binemit.

use crate::ir::Function;
use crate::machinst::*;
use crate::settings;
use crate::timing;

use log::debug;
use regalloc::{allocate_registers_with_opts, Algorithm, Options};

/// Compile the given function down to VCode with allocated registers, ready
/// for binary emission.
pub fn compile<B: LowerBackend + MachBackend>(
    f: &Function,
    b: &B,
    abi: Box<dyn ABIBody<I = B::MInst>>,
) -> CodegenResult<VCode<B::MInst>>
where
    B::MInst: ShowWithRRU,
{
    // Compute lowered block order.
    let block_order = BlockLoweringOrder::new(f);
    // Build the lowering context.
    let lower = Lower::new(f, abi, block_order)?;
    // Lower the IR.
    let mut vcode = lower.lower(b)?;

    debug!(
        "vcode from lowering: \n{}",
        vcode.show_rru(Some(b.reg_universe()))
    );

    // Perform register allocation.
    let (run_checker, algorithm) = match vcode.flags().regalloc() {
        settings::Regalloc::Backtracking => (false, Algorithm::Backtracking(Default::default())),
        settings::Regalloc::BacktrackingChecked => {
            (true, Algorithm::Backtracking(Default::default()))
        }
        settings::Regalloc::ExperimentalLinearScan => {
            (false, Algorithm::LinearScan(Default::default()))
        }
        settings::Regalloc::ExperimentalLinearScanChecked => {
            (true, Algorithm::LinearScan(Default::default()))
        }
    };

    #[cfg(feature = "regalloc-snapshot")]
    {
        use std::fs;
        use std::path::Path;
        if let Some(path) = std::env::var("SERIALIZE_REGALLOC").ok() {
            let snapshot = regalloc::IRSnapshot::from_function(&vcode, b.reg_universe());
            let serialized = bincode::serialize(&snapshot).expect("couldn't serialize snapshot");

            let file_path = Path::new(&path).join(Path::new(&format!("ir{}.bin", f.name)));
            fs::write(file_path, &serialized).expect("couldn't write IR snapshot file");
        }
    }

    let result = {
        let _tt = timing::regalloc();
        allocate_registers_with_opts(
            &mut vcode,
            b.reg_universe(),
            Options {
                run_checker,
                algorithm,
            },
        )
        .map_err(|err| {
            debug!(
                "Register allocation error for vcode\n{}\nError: {:?}",
                vcode.show_rru(Some(b.reg_universe())),
                err
            );
            err
        })
        .expect("register allocation")
    };

    // Reorder vcode into final order and copy out final instruction sequence
    // all at once. This also inserts prologues/epilogues.
    vcode.replace_insns_from_regalloc(result);

    debug!(
        "vcode after regalloc: final version:\n{}",
        vcode.show_rru(Some(b.reg_universe()))
    );

    Ok(vcode)
}
