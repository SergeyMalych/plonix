//! Crash reports, on the user's terms.
//!
//! A crash writes a scrubbed report to `$PLONIX_HOME/crashes/` (see
//! `plonix_core::crash`). Nothing is sent. On the next launch the app asks
//! once about reports it has not asked about yet: the user can read the
//! report, open a new GitHub issue with it filled in (their browser shows it
//! before anything is submitted), or dismiss it. Closing the dialog dismisses.

use std::path::Path;
use std::time::Duration;

use plonix_core::crash;
use plonix_core::paths::Home;
use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult};

/// Gives the window a moment to appear before asking.
const DELAY: Duration = Duration::from_secs(1);
const VIEW: &str = "View Report";
const REPORT: &str = "Report on GitHub";
const DISMISS: &str = "Dismiss";

/// Prints where a report went; the dialog comes on the next launch.
pub fn saved(path: &Path) {
    eprintln!("Plonix crashed. A report was saved to {} (nothing was sent)", path.display());
}

/// Asks about the newest unseen report, if there is one. Blocks until the
/// user has answered.
pub fn ask(app: &AppHandle, home: &Home) {
    let unseen = crash::unseen(home);
    let Some(newest) = unseen.first() else { return };
    let Ok(report) = std::fs::read_to_string(newest) else { return };
    std::thread::sleep(DELAY);
    let more = match unseen.len() {
        1 => String::new(),
        n => format!(" It has happened {n} times since you were last asked."),
    };
    let message = format!(
        "Plonix quit unexpectedly last time.{more} Report it?\n\n\
         Nothing has been sent. Report on GitHub opens a new issue in your browser with the report filled in, \
         for you to read before you submit it. The report has the version, your system, where Plonix stopped and why; \
         addresses, headers, tokens and your user name are removed."
    );
    loop {
        let result = app
            .dialog()
            .message(&message)
            .title("Plonix quit unexpectedly")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::YesNoCancelCustom(VIEW.into(), REPORT.into(), DISMISS.into()))
            .blocking_show_with_result();
        match choice(&result) {
            Some(VIEW) => {
                crate::open_externally(&newest.to_string_lossy());
                // Ask again, so the user can still report or dismiss it.
                continue;
            }
            Some(REPORT) => {
                let name = newest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                crate::open_externally(&crash::issue_url(&report, &name));
            }
            _ => {}
        }
        break;
    }
    if let Err(e) = crash::mark_seen(home, &unseen) {
        tracing::warn!("could not note the crash report as seen: {e}");
    }
}

/// Which button was pressed. Platforms report a custom button either by its
/// label or as the standard button it stands in for.
fn choice(result: &MessageDialogResult) -> Option<&'static str> {
    match result {
        MessageDialogResult::Custom(text) => [VIEW, REPORT, DISMISS].into_iter().find(|l| l == text),
        MessageDialogResult::Yes | MessageDialogResult::Ok => Some(VIEW),
        MessageDialogResult::No => Some(REPORT),
        MessageDialogResult::Cancel => Some(DISMISS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_map_by_label_or_position() {
        assert_eq!(choice(&MessageDialogResult::Custom(REPORT.into())), Some(REPORT));
        assert_eq!(choice(&MessageDialogResult::Custom("Something else".into())), None);
        assert_eq!(choice(&MessageDialogResult::Yes), Some(VIEW));
        assert_eq!(choice(&MessageDialogResult::No), Some(REPORT));
        assert_eq!(choice(&MessageDialogResult::Cancel), Some(DISMISS));
    }
}
