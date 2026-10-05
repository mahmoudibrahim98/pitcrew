//! Deterministic, explicit export; normal protocol tests never write bindings.
#![cfg(feature = "ts")]
use pitcrew_protocol::{api, events, ids, import, model, recap, runner, scan, transcript};
use serde::{Serialize, de::DeserializeOwned};
use std::{error::Error, fmt::Debug, fs, path::Path};
use ts_rs::{Config, TS};

fn fixture<T: TS + Serialize + DeserializeOwned + PartialEq + Debug>(
    config: &Config,
    output: &mut String,
    name: &str,
    value: T,
) -> Result<(), Box<dyn Error>> {
    let json = serde_json::to_value(&value)?;
    assert_eq!(value, serde_json::from_value::<T>(json.clone())?);
    output.push_str(&format!(
        "export const {name}: import('../index.ts').{} = {};\n",
        T::name(config),
        serde_json::to_string_pretty(&json)?
    ));
    Ok(())
}

fn write_exports(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(destination)? {
        let name = entry?.file_name();
        assert!(source.join(&name).exists(), "stale binding: {name:?}");
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            write_exports(&entry.path(), &target)?;
        } else {
            fs::write(target, fs::read(entry.path())?)?;
        }
    }
    Ok(())
}

#[test]
fn export_bindings() -> Result<(), Box<dyn Error>> {
    let destination = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/protocol-ts");
    let package = std::env::temp_dir().join(format!("pitcrew-bindings-{}", ids::EventId::new()));
    let bindings = package.join("bindings");
    fs::create_dir_all(&bindings)?;
    // serde_json writes integers as JSON numbers, never JavaScript bigint literals.
    let config = Config::new()
        .with_large_int("number")
        .with_out_dir(&bindings)
        .with_import_extension(Some("ts"));
    pitcrew_protocol::onboarding::HooksDiff::export_all(&config)?;
    pitcrew_protocol::onboarding::InstallHooks::export_all(&config)?;
    pitcrew_protocol::onboarding::SafetySettings::export_all(&config)?;
    api::ApiError::export_all(&config)?;
    pitcrew_protocol::files::FileList::export_all(&config)?;
    pitcrew_protocol::files::FileContent::export_all(&config)?;
    pitcrew_protocol::files::WriteFile::export_all(&config)?;
    // Board drafts (api-v1.md, "Board drafts").
    pitcrew_protocol::board::BoardDraft::export_all(&config)?;
    pitcrew_protocol::board::BoardProposal::export_all(&config)?;
    pitcrew_protocol::board::DraftCost::export_all(&config)?;
    pitcrew_protocol::board::DraftPreview::export_all(&config)?;
    pitcrew_protocol::board::DraftReview::export_all(&config)?;
    pitcrew_protocol::board::DraftReviewed::export_all(&config)?;
    pitcrew_protocol::board::DraftState::export_all(&config)?;
    pitcrew_protocol::board::DraftedTask::export_all(&config)?;
    pitcrew_protocol::board::ProposedTask::export_all(&config)?;
    pitcrew_protocol::board::StartDraft::export_all(&config)?;
    pitcrew_protocol::board::UsageEstimate::export_all(&config)?;
    pitcrew_protocol::ids::DraftId::export_all(&config)?;
    api::ReadCursor::export_all(&config)?;
    api::MoveCursor::export_all(&config)?;
    api::Caller::export_all(&config)?;
    api::ErrorCode::export_all(&config)?;
    api::EventsPage::export_all(&config)?;
    api::HostInfo::export_all(&config)?;
    api::HostRole::export_all(&config)?;
    api::NewProject::export_all(&config)?;
    api::NewTask::export_all(&config)?;
    api::NewWorkstream::export_all(&config)?;
    api::Setup::export_all(&config)?;
    api::SetupDone::export_all(&config)?;
    api::SetupPerson::export_all(&config)?;
    api::StreamFrame::export_all(&config)?;
    api::TokenScope::export_all(&config)?;
    events::Event::export_all(&config)?;
    events::EventBody::export_all(&config)?;
    ids::AskId::export_all(&config)?;
    ids::CommandId::export_all(&config)?;
    ids::DispatchId::export_all(&config)?;
    ids::EventId::export_all(&config)?;
    ids::MachineId::export_all(&config)?;
    ids::MemberId::export_all(&config)?;
    ids::PersonaId::export_all(&config)?;
    ids::ProjectId::export_all(&config)?;
    ids::ProjectKey::export_all(&config)?;
    ids::SessionId::export_all(&config)?;
    ids::SubtaskId::export_all(&config)?;
    ids::TaskId::export_all(&config)?;
    ids::TaskKey::export_all(&config)?;
    ids::TeamId::export_all(&config)?;
    ids::TerminalId::export_all(&config)?;
    ids::WorkspaceId::export_all(&config)?;
    ids::WorkstreamId::export_all(&config)?;
    model::Answer::export_all(&config)?;
    model::Ask::export_all(&config)?;
    model::AskKind::export_all(&config)?;
    model::AskState::export_all(&config)?;
    model::Brief::export_all(&config)?;
    model::BriefProposal::export_all(&config)?;
    model::BriefSource::export_all(&config)?;
    model::BriefTarget::export_all(&config)?;
    model::Date::export_all(&config)?;
    model::Dispatch::export_all(&config)?;
    model::DispatchOutcome::export_all(&config)?;
    model::Engine::export_all(&config)?;
    model::ExternalRef::export_all(&config)?;
    model::ExternalSystem::export_all(&config)?;
    model::Health::export_all(&config)?;
    model::LinkBasis::export_all(&config)?;
    model::Liveness::export_all(&config)?;
    model::Location::export_all(&config)?;
    model::Machine::export_all(&config)?;
    model::MachineInfo::export_all(&config)?;
    model::MachineKind::export_all(&config)?;
    model::Member::export_all(&config)?;
    model::MemberKind::export_all(&config)?;
    model::Mover::export_all(&config)?;
    model::PermissionMode::export_all(&config)?;
    model::Persona::export_all(&config)?;
    model::Priority::export_all(&config)?;
    model::Project::export_all(&config)?;
    model::ProjectStatus::export_all(&config)?;
    model::Receipt::export_all(&config)?;
    model::Scheduler::export_all(&config)?;
    model::Session::export_all(&config)?;
    model::SessionState::export_all(&config)?;
    model::Subtask::export_all(&config)?;
    model::SubtaskSource::export_all(&config)?;
    model::Task::export_all(&config)?;
    model::TaskPatch::export_all(&config)?;
    model::TaskStatus::export_all(&config)?;
    model::Team::export_all(&config)?;
    model::Workspace::export_all(&config)?;
    model::Workstream::export_all(&config)?;
    model::WorkstreamStatus::export_all(&config)?;
    recap::Block::export_all(&config)?;
    recap::BlockKey::export_all(&config)?;
    recap::BlocksPage::export_all(&config)?;
    recap::Check::export_all(&config)?;
    recap::Counts::export_all(&config)?;
    recap::DayRecap::export_all(&config)?;
    recap::DaysPage::export_all(&config)?;
    recap::Fact::export_all(&config)?;
    recap::FactKind::export_all(&config)?;
    recap::FileTouch::export_all(&config)?;
    recap::RecapBlock::export_all(&config)?;
    recap::Span::export_all(&config)?;
    recap::Summary::export_all(&config)?;
    runner::Capability::export_all(&config)?;
    runner::CommandOutcome::export_all(&config)?;
    runner::EndMode::export_all(&config)?;
    runner::HubToRunner::export_all(&config)?;
    runner::Key::export_all(&config)?;
    runner::RunnerCommand::export_all(&config)?;
    runner::RunnerToHub::export_all(&config)?;
    scan::EngineCount::export_all(&config)?;
    scan::FolderCount::export_all(&config)?;
    scan::HomeCount::export_all(&config)?;
    scan::MonthCount::export_all(&config)?;
    scan::ScanCounts::export_all(&config)?;
    scan::ScanFrame::export_all(&config)?;
    import::ImportChoice::export_all(&config)?;
    import::ImportDryRun::export_all(&config)?;
    import::ImportResult::export_all(&config)?;
    scan::ScanProgress::export_all(&config)?;
    scan::ScanReport::export_all(&config)?;
    scan::Suggestion::export_all(&config)?;
    scan::WorkstreamSuggestion::export_all(&config)?;
    transcript::PlanItem::export_all(&config)?;
    transcript::PlanStatus::export_all(&config)?;
    transcript::TranscriptItem::export_all(&config)?;
    transcript::TranscriptPage::export_all(&config)?;

    fs::write(
        bindings.join("TimestampMs.ts"),
        "// Generated by export_bindings.\nexport type TimestampMs = number;\n",
    )?;
    let mut names = fs::read_dir(&bindings)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    let mut index = String::from("// Generated by export_bindings. Do not edit.\n");
    for path in names {
        if path.extension().is_some_and(|ext| ext == "ts") {
            let name = path
                .file_stem()
                .ok_or("missing type name")?
                .to_str()
                .ok_or("non-UTF-8 type name")?;
            index.push_str(&format!(
                "export type {{ {name} }} from './bindings/{name}.ts';\n"
            ));
        }
    }
    fs::write(package.join("index.ts"), index)?;
    fs::create_dir_all(package.join("tests"))?;
    let mut examples = String::from("// Generated from Rust values; checked with tsc --noEmit.\n");
    let demo: serde_json::Value =
        serde_json::from_str(include_str!("../../fixtures/data/demo-workspace.json"))?;
    fixture(
        &config,
        &mut examples,
        "task",
        serde_json::from_value::<model::Task>(demo["tasks"][0].clone())?,
    )?;
    fixture(
        &config,
        &mut examples,
        "machine",
        serde_json::from_value::<model::Machine>(demo["machines"][0].clone())?,
    )?;
    let id: ids::TaskId = "01J00000000000000000000000".parse()?;
    fixture(&config, &mut examples, "taskId", id)?;
    fixture(
        &config,
        &mut examples,
        "event",
        events::Event {
            id: "01J00000000000000000000000".parse()?,
            at: 42,
            workspace: "01J00000000000000000000000".parse()?,
            author: "01J00000000000000000000000".parse()?,
            on_behalf_of: None,
            body: events::EventBody::TaskAssigned {
                task: id,
                assignee: None,
            },
        },
    )?;
    fixture(
        &config,
        &mut examples,
        "taskKey",
        "DEMO-1".parse::<ids::TaskKey>()?,
    )?;
    fixture(
        &config,
        &mut examples,
        "date",
        model::Date("2026-01-01".into()),
    )?;
    let patch: model::TaskPatch =
        serde_json::from_str(r#"{"workstream":null,"due":null,"title":"Clear due date"}"#)?;
    assert_eq!(
        serde_json::to_value(&patch)?["due"],
        serde_json::Value::Null
    );
    assert!(
        !serde_json::to_value(&patch)?
            .as_object()
            .ok_or("patch is not object")?
            .contains_key("start")
    );
    fixture(&config, &mut examples, "patch", patch)?;
    fixture(
        &config,
        &mut examples,
        "emptyPatch",
        model::TaskPatch::default(),
    )?;
    fixture(
        &config,
        &mut examples,
        "error",
        api::ApiError {
            code: api::ErrorCode::Forbidden,
            message: "Refused".into(),
        },
    )?;
    fixture(
        &config,
        &mut examples,
        "hello",
        api::StreamFrame::Hello {
            rev: 42,
            log: "synthetic-log".into(),
        },
    )?;
    fixture(
        &config,
        &mut examples,
        "tool",
        transcript::TranscriptItem::ToolUse {
            at: 42,
            call_id: "synthetic-call".into(),
            tool: "Read".into(),
            target: "README.md".into(),
            input: Some(serde_json::json!({"path":"README.md","nested":[null,true,2]})),
            offset: 8,
        },
    )?;
    fixture(
        &config,
        &mut examples,
        "span",
        recap::Span {
            range: 0..2,
            receipts: vec![model::Receipt::Event {
                id: "01J00000000000000000000000".parse()?,
            }],
        },
    )?;
    fixture(
        &config,
        &mut examples,
        "fact",
        recap::FactKind::SessionStarted { title: None },
    )?;
    fixture(
        &config,
        &mut examples,
        "command",
        runner::RunnerCommand::EndSession {
            session: "01J00000000000000000000000".parse()?,
            mode: runner::EndMode::Graceful,
        },
    )?;
    // A board draft waiting for review: optional fields omitted, the proposal's included.
    fixture(
        &config,
        &mut examples,
        "boardDraft",
        pitcrew_protocol::board::BoardDraft {
            id: "01J00000000000000000000000".parse()?,
            workstream: "01J00000000000000000000000".parse()?,
            agent: "01J00000000000000000000000".parse()?,
            engine: model::Engine::Claude,
            session: "01J00000000000000000000000".parse()?,
            by: "01J00000000000000000000000".parse()?,
            prompt: "draft-board/v1".into(),
            cost: pitcrew_protocol::board::DraftCost {
                sessions: 1,
                sessions_left_out: 0,
                tasks: 0,
                summary_bytes: 300,
                prompt_bytes: 2400,
                redacted: 0,
                estimate: pitcrew_protocol::board::UsageEstimate {
                    input_tokens: 15_600,
                    output_tokens: 8192,
                },
            },
            started: 42,
            state: pitcrew_protocol::board::DraftState::Proposed,
            proposal: Some(pitcrew_protocol::board::BoardProposal {
                tasks: vec![pitcrew_protocol::board::ProposedTask {
                    title: "Synthetic task".into(),
                    status: model::TaskStatus::Todo,
                    description: None,
                    evidence: vec!["01J00000000000000000000000".parse()?],
                }],
                note: None,
            }),
            proposed: Some(43),
            reviewed: None,
            accepted: Vec::new(),
            rejected: Vec::new(),
        },
    )?;
    // Request dimensions may be omitted even though Rust serializes their defaults.
    examples.push_str(
        "export const allImport: import('../index.ts').ImportFilter = { mode: 'all' };\n",
    );
    examples.push_str("export const filteredImport: import('../index.ts').ImportFilter = { mode: 'filtered', since: '2026-01-01' };\n");
    fs::write(package.join("tests/fixtures.ts"), examples)?;
    // Never delete checked-in files. A removed type needs an explicit repository change.
    write_exports(&bindings, &destination.join("bindings"))?;
    fs::write(
        destination.join("index.ts"),
        fs::read(package.join("index.ts"))?,
    )?;
    fs::write(
        destination.join("tests/fixtures.ts"),
        fs::read(package.join("tests/fixtures.ts"))?,
    )?;
    fs::remove_dir_all(package)?;
    Ok(())
}
