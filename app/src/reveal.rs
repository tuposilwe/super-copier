//! Reveals a file in the OS file manager (Finder, Explorer) — jumps
//! straight to its containing folder with the file itself selected,
//! rather than just opening the folder blind.

use std::path::Path;

#[cfg(target_os = "macos")]
pub fn reveal(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("open").arg("-R").arg(path).spawn()?;
    Ok(())
}

#[cfg(windows)]
pub fn reveal(path: &Path) -> std::io::Result<()> {
    // `explorer /select,"path"` is the standard way to open Explorer with a
    // file pre-selected. explorer.exe's own argument parsing expects the
    // quotes to sit right after the comma (not around the whole flag), so
    // we build the literal command-line text with `raw_arg` instead of
    // letting `Command` apply its normal argv-escaping — that would quote
    // the entire "/select,..." string as one token, which explorer doesn't
    // parse correctly for paths containing spaces.
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn reveal(path: &Path) -> std::io::Result<()> {
    let target = path.parent().unwrap_or(path);
    std::process::Command::new("xdg-open").arg(target).spawn()?;
    Ok(())
}
