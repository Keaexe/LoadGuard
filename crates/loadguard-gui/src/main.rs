use loadguard_core::launch;
use std::{ffi::OsString, os::unix::fs::MetadataExt, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    // Second stage of a limited launch, inside its new scope.
    let result = if args.get(1).is_some_and(|a| a == launch::STAGE) {
        launch::Stage::parse(&args[2..]).and_then(|stage| {
            let uid = std::fs::metadata("/proc/self")
                .map_err(|e| e.to_string())?
                .uid();
            Err(launch::enter(&stage, uid))
        })
    } else {
        // --hidden: started at login; stay in the tray.
        loadguard_gui::controller::run(args.get(1).is_some_and(|a| a == "--hidden"))
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
