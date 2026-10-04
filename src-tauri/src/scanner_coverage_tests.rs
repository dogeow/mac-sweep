use super::*;
use std::os::unix::fs::symlink;

struct CoverageFixture {
    home: PathBuf,
}

impl CoverageFixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mac-sweep-coverage-{}-{}",
            std::process::id(),
            NEXT_SCAN.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("Library/Caches")).unwrap();
        Self {
            home: path.canonicalize().unwrap(),
        }
    }

    fn file(&self, relative: &str) -> PathBuf {
        let path = self.home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"fixture contents").unwrap();
        path
    }

    fn old(&self, path: &Path) {
        let time = unix_now() as i64 - 40 * 86_400;
        let times = [libc::timespec {
            tv_sec: time,
            tv_nsec: 0,
        }; 2];
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: both pointers refer to live, initialized values for this call.
        assert_eq!(
            unsafe {
                libc::utimensat(
                    libc::AT_FDCWD,
                    c_path.as_ptr(),
                    times.as_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            },
            0
        );
    }

    fn old_branch(&self, relative: &str, files: usize) -> PathBuf {
        for index in 0..files {
            let file = self.file(&format!("{relative}/part-{index:04}.cache"));
            self.old(&file);
        }
        let directory = self.home.join(relative);
        self.old(&directory);
        directory
    }

    fn snapshot(&self) -> ScanSnapshot {
        ScanSnapshot {
            report: ScanReport {
                scan_id: "coverage-fixture".into(),
                started_at: unix_now(),
                duration_ms: 0,
                disk: DiskInfo {
                    total_bytes: 0,
                    available_bytes: 0,
                },
                items: vec![],
                warnings: vec![],
                installed_app_count: 0,
                scanned_files: 0,
                cancelled: false,
            },
            home: self.home.clone(),
            options: ScanOptions::default(),
            validated: HashMap::new(),
            source_counts: HashMap::new(),
        }
    }
}

impl Drop for CoverageFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn no_running_or_installed_apps() -> AppInventory {
    AppInventory {
        apps: vec![],
        complete: true,
        active_paths: HashSet::new(),
        active_complete: true,
    }
}

fn coverage_context(cancel: &AtomicBool) -> Context<'_, impl Fn(ScanProgress)> {
    coverage_context_with_progress(cancel, |_| {})
}

fn coverage_context_with_progress<F: Fn(ScanProgress)>(
    cancel: &AtomicBool,
    progress: F,
) -> Context<'_, F> {
    Context {
        cancel,
        progress,
        started: Instant::now(),
        last_progress: None,
        phase: "caches",
        visits: 0,
        files: 0,
        found: 0,
        bytes: 0,
        cutoff: unix_now() - 14 * 86_400,
        warnings: vec![],
        stopped: false,
    }
}

#[test]
fn recent_cache_parent_offers_one_complete_old_branch_instead_of_2001_file_candidates() {
    let fixture = CoverageFixture::new();
    let old_branch = fixture.old_branch("Library/Caches/demo/old-branch", 2_001);
    let recent = fixture.file("Library/Caches/demo/recent.cache");
    let root = fixture.home.join("Library/Caches");
    let start = root.join("demo");
    let cancel = AtomicBool::new(false);
    let mut ctx = coverage_context(&cancel);
    let mut snapshot = fixture.snapshot();

    walk_cache_branches(
        &mut snapshot,
        &mut ctx,
        &no_running_or_installed_apps(),
        &root,
        &start,
        Rule::Cache,
        "demo",
        None,
        0,
    );

    assert!(!ctx.stopped);
    assert_eq!(snapshot.report.items.len(), 1);
    let candidate = &snapshot.report.items[0];
    assert_eq!(Path::new(&candidate.path), old_branch);
    assert_eq!(candidate.files, 2_001);
    assert!(candidate.selected_by_default);
    let validated = snapshot.validated.get(&candidate.id).unwrap();
    assert_eq!(validated.manifest.files, 2_001);
    assert_eq!(validated.manifest.entries, 2_002);
    assert!(validate_item(&snapshot, validated, &no_running_or_installed_apps()).is_ok());
    assert!(recent.is_file());
    assert!(start.is_dir());
}

#[test]
fn protected_files_and_symlink_siblings_do_not_hide_an_independently_safe_old_branch() {
    let fixture = CoverageFixture::new();
    let old_branch = fixture.old_branch("Library/Caches/demo/safe-branch", 3);
    let model = fixture.file("Library/Caches/demo/weights.gguf");
    let database = fixture.file("Library/Caches/demo/state.sqlite");
    fixture.old(&model);
    fixture.old(&database);
    let target = fixture.file("preserved-outside-cache.txt");
    let link = fixture.home.join("Library/Caches/demo/link");
    symlink(&target, &link).unwrap();
    let recent = fixture.file("Library/Caches/demo/recent.cache");
    let root = fixture.home.join("Library/Caches");
    let start = root.join("demo");
    let cancel = AtomicBool::new(false);
    let mut ctx = coverage_context(&cancel);
    let mut snapshot = fixture.snapshot();

    walk_cache_branches(
        &mut snapshot,
        &mut ctx,
        &no_running_or_installed_apps(),
        &root,
        &start,
        Rule::Cache,
        "demo",
        None,
        0,
    );

    assert_eq!(snapshot.report.items.len(), 1);
    assert_eq!(Path::new(&snapshot.report.items[0].path), old_branch);
    assert_eq!(snapshot.report.items[0].files, 3);
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    for preserved in [model, database, target, recent] {
        assert!(preserved.is_file());
    }
}

#[test]
fn an_aggregated_directory_is_rejected_after_an_old_insert_or_nested_modification() {
    for insert in [false, true] {
        let fixture = CoverageFixture::new();
        let branch = fixture.old_branch("Library/Caches/demo/old-branch", 2);
        fixture.file("Library/Caches/demo/recent.cache");
        let root = fixture.home.join("Library/Caches");
        let cancel = AtomicBool::new(false);
        let mut ctx = coverage_context(&cancel);
        let mut snapshot = fixture.snapshot();
        walk_cache_branches(
            &mut snapshot,
            &mut ctx,
            &no_running_or_installed_apps(),
            &root,
            &root.join("demo"),
            Rule::Cache,
            "demo",
            None,
            0,
        );
        assert_eq!(snapshot.report.items.len(), 1);
        let candidate = snapshot
            .validated
            .get(&snapshot.report.items[0].id)
            .unwrap();
        assert!(validate_item(&snapshot, candidate, &no_running_or_installed_apps()).is_ok());

        let changed = if insert {
            fixture.file("Library/Caches/demo/old-branch/inserted.cache")
        } else {
            let path = branch.join("part-0000.cache");
            fs::write(&path, b"changed contents with a different size").unwrap();
            path
        };
        // Keep every mtime old: rejection must also notice the changed manifest.
        fixture.old(&changed);
        fixture.old(&branch);
        assert!(validate_item(&snapshot, candidate, &no_running_or_installed_apps()).is_err());
    }
}

#[test]
fn one_owner_reaching_its_candidate_budget_does_not_stop_another_owner() {
    let fixture = CoverageFixture::new();
    for index in 0..=MAX_SOURCE_ITEMS {
        let old = fixture.file(&format!("Library/Caches/noisy/old-{index:04}.cache"));
        fixture.old(&old);
    }
    fixture.file("Library/Caches/noisy/recent.cache");
    let second_branch = fixture.old_branch("Library/Caches/second/old-branch", 2);
    fixture.file("Library/Caches/second/recent.cache");
    let root = fixture.home.join("Library/Caches");
    let cancel = AtomicBool::new(false);
    let mut ctx = coverage_context(&cancel);
    let mut snapshot = fixture.snapshot();
    let apps = no_running_or_installed_apps();

    walk_cache_branches(
        &mut snapshot,
        &mut ctx,
        &apps,
        &root,
        &root.join("noisy"),
        Rule::Cache,
        "noisy",
        None,
        0,
    );
    assert_eq!(snapshot.report.items.len(), MAX_SOURCE_ITEMS);
    assert!(!ctx.stopped);

    walk_cache_branches(
        &mut snapshot,
        &mut ctx,
        &apps,
        &root,
        &root.join("second"),
        Rule::Cache,
        "second",
        None,
        0,
    );
    assert_eq!(snapshot.report.items.len(), MAX_SOURCE_ITEMS + 1);
    assert!(!ctx.stopped);
    assert!(snapshot
        .report
        .items
        .iter()
        .any(|item| Path::new(&item.path) == second_branch));
    assert_eq!(snapshot.validated.len(), snapshot.report.items.len());
}

#[test]
fn rebuildable_cache_validation_requires_its_exact_root_and_owner_and_manual_review() {
    let fixture = CoverageFixture::new();
    let root = fixture.home.join(".npm/_cacache");
    let branch = fixture.old_branch(".npm/_cacache/content-v2", 2);
    let cancel = AtomicBool::new(false);
    let mut ctx = coverage_context(&cancel);
    let mut snapshot = fixture.snapshot();
    let apps = no_running_or_installed_apps();

    walk_cache_branches(
        &mut snapshot,
        &mut ctx,
        &apps,
        &root,
        &branch,
        Rule::RebuildableCache,
        "npm",
        None,
        0,
    );

    assert_eq!(snapshot.report.items.len(), 1);
    let display = &snapshot.report.items[0];
    assert_eq!(display.risk, "review");
    assert!(!display.selected_by_default);
    let candidate = snapshot.validated.get(&display.id).unwrap();
    assert!(extra_root_allowed(&fixture.home, &root, "npm"));
    assert!(validate_item(&snapshot, candidate, &apps).is_ok());

    for too_broad in [fixture.home.clone(), fixture.home.join(".npm")] {
        assert!(!extra_root_allowed(&fixture.home, &too_broad, "npm"));
        let mut forged = candidate.clone();
        forged.root = too_broad;
        assert!(validate_item(&snapshot, &forged, &apps).is_err());
    }
    assert!(!extra_root_allowed(&fixture.home, &root, "fake-owner"));
    let mut wrong_owner = candidate.clone();
    wrong_owner.owner_name = "fake-owner".into();
    assert!(validate_item(&snapshot, &wrong_owner, &apps).is_err());

    let mut root_candidate = candidate.clone();
    root_candidate.path = root;
    assert!(validate_item(&snapshot, &root_candidate, &apps).is_err());
}

#[test]
fn cancellation_before_or_during_inventory_never_offers_a_partial_directory() {
    for cancelled_before_start in [false, true] {
        let fixture = CoverageFixture::new();
        let branch = fixture.old_branch("Library/Caches/demo/old-branch", 10);
        let root = fixture.home.join("Library/Caches");
        let cancel = AtomicBool::new(cancelled_before_start);
        let mut ctx = coverage_context_with_progress(&cancel, |_| {
            cancel.store(true, Ordering::Relaxed);
        });
        let mut snapshot = fixture.snapshot();

        walk_cache_branches(
            &mut snapshot,
            &mut ctx,
            &no_running_or_installed_apps(),
            &root,
            &branch,
            Rule::Cache,
            "demo",
            None,
            0,
        );

        assert!(ctx.stopped);
        assert!(snapshot.report.items.is_empty());
        assert!(snapshot.validated.is_empty());
        assert_eq!(ctx.found, 0);
        assert_eq!(ctx.bytes, 0);
        assert!(branch.is_dir());
    }
}

#[test]
fn fixed_browser_cache_keeps_cookie_databases_recent_files_and_active_browser_data() {
    let fixture = CoverageFixture::new();
    let branch = fixture.old_branch("Library/Caches/Google/Chrome/Default/old-branch", 2);
    for name in [
        "Network/Cookies",
        "History",
        "Login Data",
        "Web Data",
        "cookie.sqlite",
    ] {
        let database = fixture.file(&format!("Library/Caches/Google/Chrome/Default/{name}"));
        fs::write(&database, b"SQLite format 3\0fixture browser state").unwrap();
        fixture.old(&database);
    }
    fixture.old(
        &fixture
            .home
            .join("Library/Caches/Google/Chrome/Default/Network"),
    );
    let recent = fixture.file("Library/Caches/Google/Chrome/Default/recent.cache");
    let app_path = fixture.home.join("Applications/Google Chrome.app");
    let mut apps = no_running_or_installed_apps();
    apps.apps.push(InstalledApp {
        id: "com.google.Chrome".into(),
        name: "Google Chrome".into(),
        path: app_path.clone(),
    });
    let cancel = AtomicBool::new(false);
    let mut ctx = coverage_context(&cancel);
    let mut snapshot = fixture.snapshot();

    scan_extra_caches(&mut snapshot, &mut ctx, &apps);

    assert_eq!(snapshot.report.items.len(), 1);
    let display = &snapshot.report.items[0];
    assert_eq!(Path::new(&display.path), branch);
    assert_eq!(display.risk, "review");
    assert!(!display.selected_by_default);
    assert!(recent.is_file());
    let candidate = snapshot.validated.get(&display.id).unwrap();
    assert!(validate_item(&snapshot, candidate, &apps).is_ok());

    apps.active_paths.insert(app_path);
    assert!(validate_item(&snapshot, candidate, &apps).is_err());
    let mut active_snapshot = fixture.snapshot();
    let mut active_ctx = coverage_context(&cancel);
    scan_extra_caches(&mut active_snapshot, &mut active_ctx, &apps);
    assert!(active_snapshot.report.items.is_empty());
    assert!(active_snapshot.validated.is_empty());
}

#[test]
fn saved_application_state_is_not_an_orphan_candidate_for_an_installed_app() {
    let fixture = CoverageFixture::new();
    let id = "com.fixturevendor.present";
    let relative = format!("Library/Saved Application State/{id}.savedState/windows.plist");
    let state_file = fixture.file(&relative);
    fixture.old(&state_file);
    let directory = state_file.parent().unwrap();
    fixture.old(directory);
    let root = fixture.home.join("Library/Saved Application State");
    let installed_app = InstalledApp {
        id: id.into(),
        name: "Present".into(),
        path: fixture.home.join("Applications/Present.app"),
    };
    let mut apps = no_running_or_installed_apps();
    apps.apps.push(installed_app.clone());
    let cancel = AtomicBool::new(false);
    let mut ctx = coverage_context(&cancel);
    let mut snapshot = fixture.snapshot();

    add_candidate(
        &mut snapshot,
        &mut ctx,
        &apps,
        directory,
        &root,
        Rule::OrphanState,
        Some(id.into()),
        id,
        None,
    );
    assert!(snapshot.report.items.is_empty());
    assert!(snapshot.validated.is_empty());

    apps.apps.clear();
    apps.complete = false;
    add_candidate(
        &mut snapshot,
        &mut ctx,
        &apps,
        directory,
        &root,
        Rule::OrphanState,
        Some(id.into()),
        id,
        None,
    );
    assert!(snapshot.report.items.is_empty());

    apps.complete = true;
    add_candidate(
        &mut snapshot,
        &mut ctx,
        &apps,
        directory,
        &root,
        Rule::OrphanState,
        Some(id.into()),
        id,
        None,
    );
    assert_eq!(snapshot.report.items.len(), 1);
    let display = &snapshot.report.items[0];
    assert_eq!(display.risk, "review");
    assert!(!display.selected_by_default);
    let candidate = snapshot.validated.get(&display.id).unwrap();
    assert!(validate_item(&snapshot, candidate, &apps).is_ok());
    apps.apps.push(installed_app);
    assert!(validate_item(&snapshot, candidate, &apps).is_err());
}

#[test]
fn extra_cache_discovery_keeps_sandbox_documents_app_accounts_and_npm_configuration() {
    let fixture = CoverageFixture::new();
    let sandbox_root = fixture
        .home
        .join("Library/Containers/com.fixture.app/Data/Library/Caches");
    let sandbox_branch = fixture.old_branch(
        "Library/Containers/com.fixture.app/Data/Library/Caches/old-branch",
        2,
    );
    let support_root = fixture
        .home
        .join("Library/Application Support/FixtureApp/Code Cache");
    let support_branch = fixture.old_branch(
        "Library/Application Support/FixtureApp/Code Cache/old-branch",
        2,
    );
    let npm_root = fixture.home.join(".npm/_cacache");
    let npm_branch = fixture.old_branch(".npm/_cacache/content-v2", 2);
    let preserved = [
        fixture.file("Library/Containers/com.fixture.app/Data/Documents/private-chat.txt"),
        fixture.file("Library/Application Support/FixtureApp/Local Storage/account-state.txt"),
        fixture.file("Library/Application Support/FixtureApp/account-state.txt"),
        fixture.file(".npm/config"),
        fixture.file(".npmrc"),
    ];
    // These files are old and otherwise ordinary: scope must protect them even
    // without relying on the database or model extension exclusion.
    for path in &preserved {
        fixture.old(path);
    }
    let cancel = AtomicBool::new(false);
    let mut ctx = coverage_context(&cancel);
    let mut snapshot = fixture.snapshot();
    let apps = no_running_or_installed_apps();

    scan_extra_caches(&mut snapshot, &mut ctx, &apps);

    let expected: HashSet<_> = [sandbox_branch, support_branch, npm_branch]
        .into_iter()
        .collect();
    let actual: HashSet<_> = snapshot
        .report
        .items
        .iter()
        .map(|item| PathBuf::from(&item.path))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(snapshot.validated.len(), 3);
    assert!(!ctx.stopped);
    for item in &snapshot.report.items {
        assert_eq!(item.risk, "review");
        assert!(!item.selected_by_default);
        assert!(item.is_directory);
        assert_eq!(item.files, 2);
        let candidate = snapshot.validated.get(&item.id).unwrap();
        assert!(validate_item(&snapshot, candidate, &apps).is_ok());
    }
    for root in [sandbox_root, support_root, npm_root] {
        assert!(root.is_dir());
        assert!(!actual.contains(&root));
    }
    for path in preserved {
        assert!(path.is_file());
        assert!(!actual.contains(&path));
    }
}

#[test]
fn a_saturated_log_owner_preserves_visit_budget_and_does_not_hide_a_sibling_owner() {
    let fixture = CoverageFixture::new();
    for index in 0..2_001 {
        let batch = if index < 1_001 { "batch-a" } else { "batch-b" };
        let file = fixture.file(&format!("Library/Logs/First/{batch}/old-{index:04}.log"));
        fixture.old(&file);
    }
    let second_log = fixture.file("Library/Logs/Second/old.log");
    fixture.old(&second_log);
    let root = fixture.home.join("Library/Logs");
    let first = root.join("First");
    let second = root.join("Second");
    let cancel = AtomicBool::new(false);
    let mut ctx = coverage_context(&cancel);
    ctx.phase = "logs";
    let mut snapshot = fixture.snapshot();
    let apps = no_running_or_installed_apps();

    // Saturate First before discovering Second. Both First batches get queued,
    // so the remaining queued directory must also honor the source budget.
    walk_files(
        &mut snapshot,
        &mut ctx,
        &apps,
        &root,
        &first,
        Rule::Log,
        "First",
        None,
    );
    assert_eq!(snapshot.report.items.len(), 500);
    assert!(!ctx.stopped);

    walk_files(
        &mut snapshot,
        &mut ctx,
        &apps,
        &root,
        &root,
        Rule::Log,
        "",
        None,
    );

    assert_eq!(snapshot.report.items.len(), 501);
    assert_eq!(snapshot.validated.len(), 501);
    assert!(!ctx.stopped);
    assert!(ctx.visits < 1_100, "unexpected visits: {}", ctx.visits);
    assert_eq!(
        snapshot
            .validated
            .values()
            .filter(|item| item.owner_name == "First")
            .count(),
        500
    );
    assert!(snapshot
        .report
        .items
        .iter()
        .any(|item| Path::new(&item.path) == second_log));
    assert!(snapshot.report.items.iter().all(|item| !item.is_directory));
    for directory in [
        root,
        first.join("batch-a"),
        first.join("batch-b"),
        first,
        second,
    ] {
        assert!(directory.is_dir());
    }
}

#[test]
fn cached_tool_runtimes_and_package_downloads_never_become_default_cleanup() {
    let fixture = CoverageFixture::new();
    let root = fixture.home.join("Library/Caches");
    let owners = [
        "github-copilot-sdk",
        "com.github.wasilibs",
        "claude-cli-nodejs",
        "composer",
        "ModrinthApp",
        "Codex",
    ];
    let cancel = AtomicBool::new(false);
    let mut ctx = coverage_context(&cancel);
    let mut snapshot = fixture.snapshot();
    for owner in owners {
        let branch = fixture.old_branch(&format!("Library/Caches/{owner}"), 2);
        walk_cache_branches(
            &mut snapshot,
            &mut ctx,
            &no_running_or_installed_apps(),
            &root,
            &branch,
            Rule::Cache,
            owner,
            None,
            0,
        );
    }
    assert_eq!(snapshot.report.items.len(), owners.len());
    assert!(snapshot
        .report
        .items
        .iter()
        .all(|item| item.risk == "review" && !item.selected_by_default));
}
