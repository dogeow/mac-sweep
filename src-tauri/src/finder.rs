//! Open a fixed Finder target and transfer foreground activation from Mac Sweep.
//! No AppleScript, Accessibility automation, or persistent window-level changes.

use std::{future::Future, path::PathBuf, process::Command, time::Duration};
use tauri::AppHandle;

const FINDER_BUNDLE_ID: &str = "com.apple.finder";
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(2);
const ACTIVATION_POLL: Duration = Duration::from_millis(80);

#[derive(Clone, Copy)]
enum Target {
    Trash,
    Reveal,
}

pub async fn open_trash(app: AppHandle, home: PathBuf) -> Result<(), String> {
    show(app, home.join(".Trash"), Target::Trash).await
}

pub async fn reveal(app: AppHandle, path: PathBuf) -> Result<(), String> {
    show(app, path, Target::Reveal).await
}

async fn on_main_thread<T: Send + 'static>(
    app: &AppHandle,
    action: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (sender, mut receiver) = tauri::async_runtime::channel(1);
    app.run_on_main_thread(move || {
        let _ = sender.try_send(action());
    })
    .map_err(|error| format!("无法请求 Finder 前置：{error}"))?;
    receiver.recv().await.ok_or("Finder 前置请求未完成。")?
}

async fn wait_for_activation<F, R>(
    mut check: F,
    timeout: Duration,
    interval: Duration,
) -> Result<bool, String>
where
    F: FnMut() -> R,
    R: Future<Output = Result<bool, String>>,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if check().await? {
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let delay = interval.min(remaining);
        tauri::async_runtime::spawn_blocking(move || std::thread::sleep(delay))
            .await
            .map_err(|error| format!("Finder 前置检查未完成：{error}"))?;
    }
}

async fn show(app: AppHandle, path: PathBuf, target: Target) -> Result<(), String> {
    on_main_thread(&app, native::yield_to_finder).await?;
    let output = tauri::async_runtime::spawn_blocking(move || {
        let mut command = Command::new("/usr/bin/open");
        match target {
            Target::Trash => {
                command.arg("-b").arg(FINDER_BUNDLE_ID);
            }
            // Preserve the existing reveal semantics: select the item in Finder.
            Target::Reveal => {
                command.arg("-R");
            }
        }
        command.arg(path).output()
    })
    .await
    .map_err(|error| format!("Finder 打开请求未完成：{error}"))?
    .map_err(|error| format!("无法打开 Finder：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "无法在 Finder 中打开项目：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let accepted = on_main_thread(&app, native::request_activation).await?;
    // Activation is asynchronous, and NSRunningApplication properties are cached
    // for a main-run-loop turn. Poll in separate turns rather than treating either
    // an open exit status or an accepted activation request as foreground proof.
    let active = wait_for_activation(
        || on_main_thread(&app, native::is_finder_active),
        ACTIVATION_TIMEOUT,
        ACTIVATION_POLL,
    )
    .await?;
    let action = match target {
        Target::Trash => "open-trash",
        Target::Reveal => "reveal",
    };
    eprintln!(
        "Mac Sweep Finder handoff: action={action} request_accepted={accepted} active={active}"
    );
    if active {
        Ok(())
    } else {
        Err("已请求打开 Finder，但它未切到前台。请点击 Dock 中的 Finder 查看。".into())
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::FINDER_BUNDLE_ID;
    use objc2::{sel, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSApplicationActivationOptions, NSRunningApplication};
    use objc2_foundation::{NSObjectProtocol, NSString};

    pub fn yield_to_finder() -> Result<(), String> {
        let main = MainThreadMarker::new().ok_or("Finder 前置必须在主线程执行。")?;
        let app = NSApplication::sharedApplication(main);
        // This cooperative activation API was added in macOS 14. Runtime selector
        // checks keep the same binary valid on the supported macOS 12 and 13.
        if app.respondsToSelector(sel!(yieldActivationToApplicationWithBundleIdentifier:)) {
            app.yieldActivationToApplicationWithBundleIdentifier(&NSString::from_str(
                FINDER_BUNDLE_ID,
            ));
        }
        Ok(())
    }

    pub fn request_activation() -> Result<bool, String> {
        let main = MainThreadMarker::new().ok_or("Finder 前置必须在主线程执行。")?;
        let app = NSApplication::sharedApplication(main);
        let applications = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(FINDER_BUNDLE_ID),
        );
        let Some(finder) = applications.firstObject() else {
            // Finder may still be launching. The subsequent active-state checks
            // can confirm its activation once Launch Services finishes starting it.
            return Ok(false);
        };
        let _ = finder.unhide();
        if app.respondsToSelector(sel!(yieldActivationToApplication:))
            && finder.respondsToSelector(sel!(activateFromApplication:options:))
        {
            app.yieldActivationToApplication(&finder);
            Ok(finder.activateFromApplication_options(
                &NSRunningApplication::currentApplication(),
                NSApplicationActivationOptions::ActivateAllWindows,
            ))
        } else {
            Ok(legacy_activation(&finder))
        }
    }

    #[allow(deprecated)]
    fn legacy_activation(finder: &NSRunningApplication) -> bool {
        // IgnoringOtherApps is effective on macOS 12/13, but has no effect from
        // macOS 14 onwards; those systems use cooperative activation above.
        finder.activateWithOptions(
            NSApplicationActivationOptions::ActivateAllWindows
                | NSApplicationActivationOptions::ActivateIgnoringOtherApps,
        )
    }

    pub fn is_finder_active() -> Result<bool, String> {
        let _main = MainThreadMarker::new().ok_or("Finder 状态必须在主线程读取。")?;
        let applications = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(FINDER_BUNDLE_ID),
        );
        Ok(applications.iter().any(|finder| finder.isActive()))
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    pub fn yield_to_finder() -> Result<(), String> {
        Err("Finder 前置仅适用于 macOS。".into())
    }
    pub fn request_activation() -> Result<bool, String> {
        Err("Finder 前置仅适用于 macOS。".into())
    }
    pub fn is_finder_active() -> Result<bool, String> {
        Err("Finder 状态仅适用于 macOS。".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::future::ready;

    #[test]
    fn an_opened_but_inactive_finder_is_not_confirmed() {
        let active = tauri::async_runtime::block_on(wait_for_activation(
            || ready(Ok(false)),
            Duration::ZERO,
            Duration::from_millis(1),
        ))
        .unwrap();
        assert!(!active);
    }

    #[test]
    fn activation_is_confirmed_after_delayed_native_state_changes() {
        let mut states = VecDeque::from([false, false, true]);
        let active = tauri::async_runtime::block_on(wait_for_activation(
            || ready(Ok(states.pop_front().unwrap())),
            Duration::from_secs(1),
            Duration::from_millis(1),
        ))
        .unwrap();
        assert!(active);
        assert!(states.is_empty());
    }

    #[test]
    fn native_state_errors_are_not_reported_as_success() {
        let result = tauri::async_runtime::block_on(wait_for_activation(
            || ready(Err("main thread unavailable".into())),
            Duration::from_secs(1),
            Duration::from_millis(1),
        ));
        assert_eq!(result, Err("main thread unavailable".into()));
    }
}
