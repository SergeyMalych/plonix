//! Plonix.app carries the `plonix` command line tool as a Tauri sidecar
//! (`bundle.externalBin`: binaries/plonix-cli-<target>), and Tauri's build
//! step stops when that file is missing. Release and CI builds put the real
//! binary there first (see .github/workflows). Every other build gets a
//! stand-in script, so `cargo build` and `cargo run -p plonix-app` work
//! without it; the app recognizes the stand-in (src/cli_tool.rs).

use std::path::PathBuf;

const PLACEHOLDER: &str = "#!/bin/sh\n\
# plonix-cli placeholder: this build of Plonix does not include the command line tool.\n\
echo 'This build of Plonix does not include the plonix command. Install it with: cargo install --path crates/plonix-cli' >&2\n\
exit 1\n";

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    let ext = if target.contains("windows") { ".exe" } else { "" };
    let sidecar = PathBuf::from("binaries").join(format!("plonix-cli-{target}{ext}"));
    if !sidecar.exists() {
        std::fs::create_dir_all("binaries").expect("creating crates/plonix-app/binaries");
        std::fs::write(&sidecar, PLACEHOLDER).expect("writing the plonix-cli placeholder");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755));
        }
    }
    tauri_build::build()
}
