use micu_devtools::{Result, engineering::stress};
use std::path::Path;

#[test]
fn actual_mcp_processes_parallelize_standard_images_and_serialize_quality_requests() -> Result<()> {
    let report = stress(Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")), 4, 1)?;
    assert_eq!(report["phases"][0]["captured_requests"], 20);
    assert_eq!(report["phases"][0]["saved_images"], 20);
    assert_eq!(report["phases"][1]["captured_requests"], 4);
    assert_eq!(report["phases"][1]["max_active"], 1);
    Ok(())
}
