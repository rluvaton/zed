use std::sync::Arc;

use db::kvp::Dismissable;
use editor::Editor;
use gpui::{Context, Entity, EventEmitter, Subscription, WeakEntity};
use language::Buffer;
use lsp::NumberOrString;
use project::{Project, ProjectPath, WorktreeId};
use ui::{Banner, FluentBuilder as _, Severity, prelude::*};
use util::rel_path::RelPath;
use workspace::{ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace};

const UNLINKED_FILE_DIAGNOSTIC_CODE: &str = "unlinked-file";

pub struct RustUnlinkedFileBanner {
    pub(crate) dismissed: bool,
    pub(crate) has_unlinked_diagnostic: bool,
    pub(crate) current_file_info: Option<UnlinkedFileInfo>,
    active_buffer: Option<Entity<Buffer>>,
    project: WeakEntity<Project>,
    _subscriptions: Vec<Subscription>,
}

pub(crate) struct UnlinkedFileInfo {
    pub(crate) file_stem: String,
    parent_module_rel_path: Arc<RelPath>,
    pub(crate) parent_module_file_name: String,
    worktree_id: WorktreeId,
}

impl Dismissable for RustUnlinkedFileBanner {
    const KEY: &str = "rust-unlinked-file-banner";
}

impl RustUnlinkedFileBanner {
    pub fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let project = workspace.project().downgrade();

        let diagnostics_subscription = cx.subscribe(workspace.project(), |this, _, event, cx| {
            if let project::Event::DiagnosticsUpdated { .. } = event {
                this.refresh_diagnostics(cx);
            }
        });

        Self {
            dismissed: Self::dismissed(),
            has_unlinked_diagnostic: false,
            current_file_info: None,
            active_buffer: None,
            project,
            _subscriptions: vec![diagnostics_subscription],
        }
    }

    fn refresh_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.has_unlinked_diagnostic = false;
        self.current_file_info = None;

        let Some(buffer) = &self.active_buffer else {
            return;
        };

        let buffer_ref = buffer.read(cx);
        let Some(file) = buffer_ref.file() else {
            return;
        };

        let rel_path = file.path().clone();
        if rel_path.extension() != Some("rs") {
            return;
        }

        let worktree_id = file.worktree_id(cx);

        let snapshot = buffer_ref.snapshot();
        let has_unlinked = snapshot
            .diagnostics_in_range::<usize, usize>(0..snapshot.len(), false)
            .any(|entry| {
                matches!(
                    &entry.diagnostic.code,
                    Some(NumberOrString::String(code)) if code == UNLINKED_FILE_DIAGNOSTIC_CODE
                )
            });

        if has_unlinked {
            self.has_unlinked_diagnostic = true;

            if let Some(file_stem) = rel_path.file_stem().map(|s| s.to_string()) {
                if let Some(project) = self.project.upgrade() {
                    let project = project.read(cx);
                    if let Some(parent_info) =
                        find_parent_module(&rel_path, worktree_id, project, cx)
                    {
                        self.current_file_info = Some(UnlinkedFileInfo {
                            file_stem,
                            parent_module_file_name: parent_info.0,
                            parent_module_rel_path: parent_info.1,
                            worktree_id,
                        });
                    }
                }
            }
        }

        let location = self.toolbar_location();
        cx.emit(ToolbarItemEvent::ChangeLocation(location));
        cx.notify();
    }

    pub(crate) fn toolbar_location(&self) -> ToolbarItemLocation {
        if self.dismissed || !self.has_unlinked_diagnostic {
            ToolbarItemLocation::Hidden
        } else {
            ToolbarItemLocation::Secondary
        }
    }

    pub(crate) fn attach_to_parent_module(&mut self, cx: &mut Context<Self>) {
        let Some(info) = self.current_file_info.take() else {
            return;
        };
        let Some(project) = self.project.upgrade() else {
            return;
        };

        let file_stem = info.file_stem;
        let parent_module_rel_path = info.parent_module_rel_path;
        let worktree_id = info.worktree_id;

        cx.spawn(async move |this, cx| {
            let project_path = ProjectPath {
                worktree_id,
                path: parent_module_rel_path,
            };

            let buffer: Entity<Buffer> = project
                .update(cx, |project, cx| {
                    project
                        .buffer_store()
                        .update(cx, |store, cx| store.open_buffer(project_path, cx))
                })
                .await?;

            let mod_declaration = format!("mod {file_stem};\n");

            buffer.update(cx, |buffer, cx| {
                let contents = buffer.text();
                if contents.contains(&format!("mod {file_stem};")) {
                    return;
                }

                let insert_offset = find_mod_insertion_offset(&contents);
                buffer.edit([(insert_offset..insert_offset, mod_declaration)], None, cx);
            });

            project
                .update(cx, |project, cx| project.save_buffer(buffer, cx))
                .await?;

            this.update(cx, |this, cx| {
                this.has_unlinked_diagnostic = false;
                this.current_file_info = None;
                cx.emit(ToolbarItemEvent::ChangeLocation(ToolbarItemLocation::Hidden));
                cx.notify();
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }
}

fn find_parent_module(
    file_rel_path: &RelPath,
    worktree_id: WorktreeId,
    project: &Project,
    cx: &App,
) -> Option<(String, Arc<RelPath>)> {
    let worktree = project.worktree_for_id(worktree_id, cx)?;
    let worktree = worktree.read(cx);

    let parent = file_rel_path.parent()?;

    let candidates = ["mod.rs", "lib.rs", "main.rs"];
    for candidate_name in &candidates {
        let candidate_path = if parent.is_empty() {
            Arc::from(RelPath::unix(candidate_name).ok()?)
        } else {
            let full = format!("{}/{}", parent.as_unix_str(), candidate_name);
            Arc::from(RelPath::unix(&full).ok()?)
        };
        if worktree.entry_for_path(&candidate_path).is_some() {
            return Some((candidate_name.to_string(), candidate_path));
        }
    }

    // New-style module: if file is at `foo/bar.rs`, parent module might be `foo.rs`
    if let Some(dir_name) = parent.file_name() {
        if let Some(grandparent) = parent.parent() {
            let parent_module_name = format!("{dir_name}.rs");
            let candidate_path = if grandparent.is_empty() {
                Arc::from(RelPath::unix(&parent_module_name).ok()?)
            } else {
                let full = format!("{}/{}", grandparent.as_unix_str(), parent_module_name);
                Arc::from(RelPath::unix(&full).ok()?)
            };
            if worktree.entry_for_path(&candidate_path).is_some() {
                return Some((parent_module_name, candidate_path));
            }
        }
    }

    None
}

fn find_mod_insertion_offset(contents: &str) -> usize {
    let mut last_mod_end = None;

    for (idx, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("mod ") && trimmed.ends_with(';') {
            let line_start: usize = contents
                .lines()
                .take(idx)
                .map(|l| l.len() + 1)
                .sum();
            last_mod_end = Some(line_start + line.len() + 1);
        }
    }

    if let Some(offset) = last_mod_end {
        return offset.min(contents.len());
    }

    0
}

impl EventEmitter<ToolbarItemEvent> for RustUnlinkedFileBanner {}

impl Render for RustUnlinkedFileBanner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_attach_action = self.current_file_info.is_some();
        let parent_file_name = self
            .current_file_info
            .as_ref()
            .map(|info| info.parent_module_file_name.clone())
            .unwrap_or_else(|| "mod.rs".to_string());

        div()
            .id("rust-unlinked-file-banner")
            .when(!self.dismissed && self.has_unlinked_diagnostic, |el| {
                el.child(
                    Banner::new()
                        .severity(Severity::Warning)
                        .child(Label::new(
                            "Module declaration missing. This may impact smart editing features and auto-completion.",
                        ))
                        .action_slot(
                            h_flex()
                                .gap_0p5()
                                .when(has_attach_action, |el| {
                                    el.child(
                                        Button::new(
                                            "attach-to-mod",
                                            format!("Attach file to {parent_file_name}"),
                                        )
                                        .label_size(LabelSize::Small)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.attach_to_parent_module(cx);
                                        })),
                                    )
                                })
                                .child(
                                    IconButton::new("dismiss", IconName::Close)
                                        .icon_size(IconSize::Small)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.dismissed = true;
                                            Self::set_dismissed(true, cx);
                                            cx.emit(ToolbarItemEvent::ChangeLocation(
                                                ToolbarItemLocation::Hidden,
                                            ));
                                            cx.notify();
                                        })),
                                ),
                        )
                        .into_any_element(),
                )
            })
    }
}

impl ToolbarItemView for RustUnlinkedFileBanner {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn workspace::ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        self.has_unlinked_diagnostic = false;
        self.current_file_info = None;
        self.active_buffer = None;

        if self.dismissed {
            return ToolbarItemLocation::Hidden;
        }

        if let Some(item) = active_pane_item
            && let Some(editor) = item.act_as::<Editor>(cx)
        {
            let buffer = editor.read(cx).buffer().read(cx).as_singleton();
            if let Some(buffer) = buffer {
                let is_rust = buffer
                    .read(cx)
                    .file()
                    .is_some_and(|f| f.path().extension() == Some("rs"));

                if is_rust {
                    self.active_buffer = Some(buffer);
                    self.refresh_diagnostics(cx);
                }
            }
        }

        self.toolbar_location()
    }
}
