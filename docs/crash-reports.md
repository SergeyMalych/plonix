# Crash reports

If Plonix crashes, it writes a report on your computer and sends nothing. There is no crash-reporting service: you decide whether a report goes anywhere, and you see it first.

## What happens when Plonix crashes

The app, the `plonix` command and every engine thread (the proxy, the recorder, the local API) write a report when they hit an internal error. Reports go in `~/.plonix/crashes/` (or `$PLONIX_HOME/crashes/`), one text file per crash, readable only by you. Plonix keeps the 20 newest and writes at most three per run, so an error that repeats doesn't fill the folder.

- **In the app**, the next time Plonix starts it asks once: *Plonix quit unexpectedly last time. Report it?*
  - **View Report** opens the report in your text editor, then asks again.
  - **Report on GitHub** opens a new issue on [github.com/SergeyMalych/plonix](https://github.com/SergeyMalych/plonix/issues) in your browser, with the report filled in. Nothing is submitted until you read it and press **Submit**. Very long reports are cut to fit in the link; the full one stays in the file.
  - **Dismiss**, or closing the dialog, means no. Plonix doesn't ask about that report again.
- **In a terminal**, `plonix` prints where it saved the report. A project running in the background (`plonix start`) prints it to `~/.plonix/logs/engine.log`. To report it, read the file and paste it into a [new issue](https://github.com/SergeyMalych/plonix/issues/new).

## What a report contains

- the Plonix version, which part crashed (app or command line), the operating system and processor type, and the time
- the name of the thread, the error message and where in the Plonix source it happened
- the backtrace: the chain of functions that led there

Plonix adds nothing else: no captured traffic, settings or files. Only the error message can quote what Plonix was working on, which is why the next step cleans it.

## What is removed

Before a report is written, Plonix removes anything in it that looks like:

- the query and fragment of a URL (`https://shop.example/orders?id=7` becomes `https://shop.example/orders?<redacted>`), and user names and passwords in URLs
- header values (`("Cookie", "<redacted>")`), cookies, `Authorization` values and bearer tokens
- values of anything named like a token, secret, password, key, session or signature (`api_key=<redacted>`)
- JSON Web Tokens and other long strings of mixed letters and digits
- email addresses
- your home folder and user name (`/Users/you/…` becomes `~/…`)

The removal errs on the side of hiding too much. The report is plain text, so you can read every word before deciding to share it, and edit the issue before you submit it.
