use std::path::Path;

use fs::Fs as _;
use gpui::{AppContext as _, TestAppContext, VisualTestContext};
use language::DiagnosticSourceKind;
use lsp::LanguageServerId;
use project::{FakeFs, Project};
use serde_json::json;
use settings::SettingsStore;
use util::{path, rel_path::rel_path};
use workspace::{MultiWorkspace, ToolbarItemLocation};

use crate::RustUnlinkedFileBanner;

fn init_test(cx: &mut TestAppContext) {
    cx.update(|cx| {
        zlog::init_test();
        let settings = SettingsStore::test(cx);
        cx.set_global(settings);
        theme::init(theme::LoadThemes::JustBase, cx);
        editor::init(cx);
    });
}

#[gpui::test]
async fn test_banner_shows_for_unlinked_rust_file(cx: &mut TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/project"),
        json!({
            "src": {
                "lib.rs": "// root module\n",
                "orphan.rs": "fn hello() {}\n",
            }
        }),
    )
    .await;

    let language_server_id = LanguageServerId(0);
    let project = Project::test(fs.clone(), [path!("/project").as_ref()], cx).await;
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());

    let window =
        cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
    let cx = &mut VisualTestContext::from_window(window.into(), cx);

    let workspace = window
        .read_with(cx, |mw, _| mw.workspace().clone())
        .unwrap();

    // Add the banner toolbar item
    workspace.update_in(cx, |workspace, window, cx| {
        let toolbar = workspace.active_pane().read(cx).toolbar().clone();
        toolbar.update(cx, |toolbar, cx| {
            let banner = cx.new(|cx| RustUnlinkedFileBanner::new(workspace, cx));
            toolbar.add_item(banner, window, cx);
        });
    });

    let worktree_id = project.read_with(cx, |project, cx| {
        project.worktrees(cx).next().unwrap().read(cx).id()
    });

    // Open the unlinked Rust file
    workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_path(
                (worktree_id, rel_path("src/orphan.rs")),
                None,
                true,
                window,
                cx,
            )
        })
        .await
        .unwrap();
    cx.executor().run_until_parked();

    // Verify the banner is hidden initially (no diagnostics yet)
    let banner = workspace.read_with(cx, |workspace, cx| {
        let toolbar = workspace.active_pane().read(cx).toolbar().clone();
        toolbar
            .read(cx)
            .item_of_type::<RustUnlinkedFileBanner>()
            .unwrap()
    });

    assert!(
        !banner.read_with(cx, |b, _| b.has_unlinked_diagnostic),
        "Banner should be hidden when there are no diagnostics"
    );

    // Inject "unlinked-file" diagnostic for orphan.rs
    let orphan_uri = lsp::Uri::from_file_path(path!("/project/src/orphan.rs")).unwrap();
    lsp_store
        .update(cx, |lsp_store, cx| {
            lsp_store.update_diagnostics(
                language_server_id,
                lsp::PublishDiagnosticsParams {
                    uri: orphan_uri.clone(),
                    diagnostics: vec![lsp::Diagnostic {
                        range: lsp::Range::new(
                            lsp::Position::new(0, 0),
                            lsp::Position::new(0, 14),
                        ),
                        severity: Some(lsp::DiagnosticSeverity::WARNING),
                        code: Some(lsp::NumberOrString::String(
                            "unlinked-file".to_string(),
                        )),
                        source: Some("rust-analyzer".to_string()),
                        message: "file not included in module tree".to_string(),
                        ..Default::default()
                    }],
                    version: None,
                },
                None,
                DiagnosticSourceKind::Pushed,
                &[],
                cx,
            )
        })
        .expect("Failed to update diagnostics");

    cx.executor().run_until_parked();

    // Verify the banner detected the diagnostic
    let (has_diagnostic, has_file_info) = banner.read_with(cx, |banner, _| {
        (
            banner.has_unlinked_diagnostic,
            banner.current_file_info.is_some(),
        )
    });
    assert!(
        has_diagnostic,
        "Banner should detect the unlinked-file diagnostic"
    );
    assert!(
        has_file_info,
        "Banner should find lib.rs as the parent module"
    );

    // Verify parent module is lib.rs
    let parent_name = banner.read_with(cx, |banner, _| {
        banner
            .current_file_info
            .as_ref()
            .map(|info| info.parent_module_file_name.clone())
    });
    assert_eq!(parent_name, Some("lib.rs".to_string()));
}

#[gpui::test]
async fn test_attach_adds_mod_declaration(cx: &mut TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/project"),
        json!({
            "src": {
                "lib.rs": "mod existing;\n",
                "existing.rs": "",
                "orphan.rs": "fn hello() {}\n",
            }
        }),
    )
    .await;

    let language_server_id = LanguageServerId(0);
    let project = Project::test(fs.clone(), [path!("/project").as_ref()], cx).await;
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());

    let window =
        cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
    let cx = &mut VisualTestContext::from_window(window.into(), cx);

    let workspace = window
        .read_with(cx, |mw, _| mw.workspace().clone())
        .unwrap();

    workspace.update_in(cx, |workspace, window, cx| {
        let toolbar = workspace.active_pane().read(cx).toolbar().clone();
        toolbar.update(cx, |toolbar, cx| {
            let banner = cx.new(|cx| RustUnlinkedFileBanner::new(workspace, cx));
            toolbar.add_item(banner, window, cx);
        });
    });

    let worktree_id = project.read_with(cx, |project, cx| {
        project.worktrees(cx).next().unwrap().read(cx).id()
    });

    // Open orphan.rs
    workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_path(
                (worktree_id, rel_path("src/orphan.rs")),
                None,
                true,
                window,
                cx,
            )
        })
        .await
        .unwrap();
    cx.executor().run_until_parked();

    // Inject "unlinked-file" diagnostic
    let orphan_uri = lsp::Uri::from_file_path(path!("/project/src/orphan.rs")).unwrap();
    lsp_store
        .update(cx, |lsp_store, cx| {
            lsp_store.update_diagnostics(
                language_server_id,
                lsp::PublishDiagnosticsParams {
                    uri: orphan_uri.clone(),
                    diagnostics: vec![lsp::Diagnostic {
                        range: lsp::Range::new(
                            lsp::Position::new(0, 0),
                            lsp::Position::new(0, 14),
                        ),
                        severity: Some(lsp::DiagnosticSeverity::WARNING),
                        code: Some(lsp::NumberOrString::String(
                            "unlinked-file".to_string(),
                        )),
                        source: Some("rust-analyzer".to_string()),
                        message: "file not included in module tree".to_string(),
                        ..Default::default()
                    }],
                    version: None,
                },
                None,
                DiagnosticSourceKind::Pushed,
                &[],
                cx,
            )
        })
        .expect("Failed to update diagnostics");

    cx.executor().run_until_parked();

    // Click "Attach file to lib.rs"
    let banner = workspace.read_with(cx, |workspace, cx| {
        let toolbar = workspace.active_pane().read(cx).toolbar().clone();
        toolbar
            .read(cx)
            .item_of_type::<RustUnlinkedFileBanner>()
            .unwrap()
    });

    banner.update_in(cx, |banner, _, cx| {
        banner.attach_to_parent_module(cx);
    });
    cx.executor().run_until_parked();

    // Verify that lib.rs now contains "mod orphan;"
    let lib_contents = fs
        .load(Path::new(path!("/project/src/lib.rs")))
        .await
        .unwrap();
    assert!(
        lib_contents.contains("mod orphan;"),
        "lib.rs should contain 'mod orphan;', got: {lib_contents:?}"
    );
    assert!(
        lib_contents.contains("mod existing;"),
        "lib.rs should still contain 'mod existing;'"
    );
}

#[gpui::test]
async fn test_banner_hidden_for_non_rust_files(cx: &mut TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/project"),
        json!({
            "readme.txt": "hello",
        }),
    )
    .await;

    let project = Project::test(fs.clone(), [path!("/project").as_ref()], cx).await;

    let window =
        cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
    let cx = &mut VisualTestContext::from_window(window.into(), cx);

    let workspace = window
        .read_with(cx, |mw, _| mw.workspace().clone())
        .unwrap();

    workspace.update_in(cx, |workspace, window, cx| {
        let toolbar = workspace.active_pane().read(cx).toolbar().clone();
        toolbar.update(cx, |toolbar, cx| {
            let banner = cx.new(|cx| RustUnlinkedFileBanner::new(workspace, cx));
            toolbar.add_item(banner, window, cx);
        });
    });

    let worktree_id = project.read_with(cx, |project, cx| {
        project.worktrees(cx).next().unwrap().read(cx).id()
    });

    // Open a non-Rust file
    workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_path(
                (worktree_id, rel_path("readme.txt")),
                None,
                true,
                window,
                cx,
            )
        })
        .await
        .unwrap();
    cx.executor().run_until_parked();

    let banner = workspace.read_with(cx, |workspace, cx| {
        let toolbar = workspace.active_pane().read(cx).toolbar().clone();
        toolbar
            .read(cx)
            .item_of_type::<RustUnlinkedFileBanner>()
            .unwrap()
    });

    assert!(
        !banner.read_with(cx, |b, _| b.has_unlinked_diagnostic),
        "Banner should not show for non-Rust files"
    );
}

#[gpui::test]
async fn test_dismiss_banner(cx: &mut TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/project"),
        json!({
            "src": {
                "lib.rs": "",
                "orphan.rs": "fn hello() {}\n",
            }
        }),
    )
    .await;

    let language_server_id = LanguageServerId(0);
    let project = Project::test(fs.clone(), [path!("/project").as_ref()], cx).await;
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());

    let window =
        cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
    let cx = &mut VisualTestContext::from_window(window.into(), cx);

    let workspace = window
        .read_with(cx, |mw, _| mw.workspace().clone())
        .unwrap();

    workspace.update_in(cx, |workspace, window, cx| {
        let toolbar = workspace.active_pane().read(cx).toolbar().clone();
        toolbar.update(cx, |toolbar, cx| {
            let banner = cx.new(|cx| RustUnlinkedFileBanner::new(workspace, cx));
            toolbar.add_item(banner, window, cx);
        });
    });

    let worktree_id = project.read_with(cx, |project, cx| {
        project.worktrees(cx).next().unwrap().read(cx).id()
    });

    // Open orphan.rs and inject diagnostic
    workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_path(
                (worktree_id, rel_path("src/orphan.rs")),
                None,
                true,
                window,
                cx,
            )
        })
        .await
        .unwrap();
    cx.executor().run_until_parked();

    let orphan_uri = lsp::Uri::from_file_path(path!("/project/src/orphan.rs")).unwrap();
    lsp_store
        .update(cx, |lsp_store, cx| {
            lsp_store.update_diagnostics(
                language_server_id,
                lsp::PublishDiagnosticsParams {
                    uri: orphan_uri.clone(),
                    diagnostics: vec![lsp::Diagnostic {
                        range: lsp::Range::new(
                            lsp::Position::new(0, 0),
                            lsp::Position::new(0, 14),
                        ),
                        severity: Some(lsp::DiagnosticSeverity::WARNING),
                        code: Some(lsp::NumberOrString::String(
                            "unlinked-file".to_string(),
                        )),
                        source: Some("rust-analyzer".to_string()),
                        message: "file not included in module tree".to_string(),
                        ..Default::default()
                    }],
                    version: None,
                },
                None,
                DiagnosticSourceKind::Pushed,
                &[],
                cx,
            )
        })
        .expect("Failed to update diagnostics");

    cx.executor().run_until_parked();

    let banner = workspace.read_with(cx, |workspace, cx| {
        let toolbar = workspace.active_pane().read(cx).toolbar().clone();
        toolbar
            .read(cx)
            .item_of_type::<RustUnlinkedFileBanner>()
            .unwrap()
    });

    // Verify banner is shown
    assert!(banner.read_with(cx, |b, _| b.has_unlinked_diagnostic));

    // Dismiss the banner
    banner.update(cx, |banner, cx| {
        banner.dismissed = true;
        cx.notify();
    });
    cx.executor().run_until_parked();

    // Verify banner reports dismissed
    assert!(
        banner.read_with(cx, |b, _| b.dismissed),
        "Banner should be dismissed"
    );
    assert_eq!(
        banner.read_with(cx, |b, _| b.toolbar_location()),
        ToolbarItemLocation::Hidden,
        "Dismissed banner should report Hidden location"
    );
}
