//! LibFuzzer target for `zerovdd-config` strict TOML parsing and semantic validation.

#![no_main]

use libfuzzer_sys::fuzz_target;
use zerovdd_config::ConfigV1;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(cfg) = ConfigV1::from_toml_str(text) {
            let _ = cfg.validate();
            if let Ok(serialized) = cfg.to_toml_string() {
                let reparsed = ConfigV1::from_toml_str(&serialized)
                    .expect("serialized ConfigV1 must re-parse cleanly");
                assert_eq!(reparsed, cfg);
            }
        }
    }

    let lossy = String::from_utf8_lossy(data);
    if let Ok(cfg) = ConfigV1::from_toml_str(&lossy) {
        let _ = cfg.validate();
    }
});
