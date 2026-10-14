//! Worker-side title command relay.
//!
//! The title editor lives on the UI thread (see `fm_ui_egui`'s Titles tab);
//! this relay owns the [`TitleProtocolBridge`] next to the protocol client.
//! It records each queued title command, maps its [`CommandResult`] once the
//! client has it, releases held replies as the replicated model advances,
//! and abandons commands whose answer can no longer be trusted to arrive.
//! Everything it produces queues as [`TitleUpdate`]s that the next published
//! UI state carries.

use fm_client::{Client, CommandStatus};
use fm_protocol::CommandResult;
use fm_ui_egui::TitleUpdate;
use fm_ui_model::title_editor::{BridgeIntake, TitleCommand, TitleProtocolBridge};

#[derive(Debug)]
struct Tracked {
    command_id: String,
    request: u64,
}

#[derive(Debug, Default)]
pub(crate) struct TitleRelay {
    bridge: TitleProtocolBridge,
    /// Tracked commands without a mapped result, in send order.
    in_flight: Vec<Tracked>,
    /// Requests whose result the bridge holds until the model reaches its
    /// revision.
    held: Vec<u64>,
    /// Held requests already abandoned to the editor; their late release
    /// is dropped.
    orphaned: Vec<u64>,
    outbox: Vec<TitleUpdate>,
}

impl TitleRelay {
    /// Records that `command` was queued under protocol `command_id`.
    pub(crate) fn track(&mut self, command_id: &str, command: &TitleCommand) {
        self.bridge.track(command_id, command);
        self.in_flight.push(Tracked {
            command_id: command_id.to_owned(),
            request: command.request,
        });
    }

    /// A title intent that never reached the client queue.
    pub(crate) fn unsent(&mut self, request: u64) {
        self.outbox.push(TitleUpdate::Abandoned { request });
    }

    fn untrack(&mut self, command_id: &str) -> Option<u64> {
        let index = self
            .in_flight
            .iter()
            .position(|tracked| tracked.command_id == command_id)?;
        Some(self.in_flight.remove(index).request)
    }

    fn apply_result(&mut self, result: &CommandResult, client: &Client) {
        let id = match result {
            CommandResult::Accepted { id, .. } | CommandResult::Rejected { id, .. } => id,
        };
        let request = self.untrack(id);
        match self.bridge.apply_result(result, client.model()) {
            BridgeIntake::Reply(reply) => self.outbox.push(TitleUpdate::Reply(reply)),
            BridgeIntake::Held => self.held.extend(request),
            BridgeIntake::NotTitle => {}
        }
    }

    fn abandon(&mut self, command_id: &str) {
        self.untrack(command_id);
        if let Some(request) = self.bridge.abandon(command_id) {
            self.outbox.push(TitleUpdate::Abandoned { request });
        }
    }

    /// The command ids of tracked commands with their client status.
    fn statuses<'a>(&self, client: &'a Client) -> Vec<(String, Option<&'a CommandStatus>)> {
        self.in_flight
            .iter()
            .map(|tracked| {
                (
                    tracked.command_id.clone(),
                    client
                        .command(&tracked.command_id)
                        .map(|record| &record.status),
                )
            })
            .collect()
    }

    /// Resolves every tracked command except `keep`: the worker pursues at
    /// most one command, so any other one is mapped if the client completed
    /// it and abandoned otherwise.
    pub(crate) fn settle(&mut self, client: &Client, keep: Option<&str>) {
        for (id, status) in self.statuses(client) {
            if Some(id.as_str()) == keep {
                continue;
            }
            match status {
                Some(CommandStatus::Completed(result)) => self.apply_result(result, client),
                _ => self.abandon(&id),
            }
        }
    }

    /// Maps every result the client has completed and abandons commands that
    /// are terminally uncertain or forgotten. Without a `ready` connection
    /// every unanswered command (sent, or held for its revision) is
    /// abandoned: the editor keeps its drafts and the next snapshot shows
    /// whether the command took effect. Then releases held replies the
    /// replicated model has reached and drains the queued updates in order.
    pub(crate) fn observe(&mut self, client: &Client, ready: bool) -> Vec<TitleUpdate> {
        for (id, status) in self.statuses(client) {
            match status {
                Some(CommandStatus::Completed(result)) => self.apply_result(result, client),
                Some(CommandStatus::TerminalUncertain(_)) | None => self.abandon(&id),
                Some(CommandStatus::Queued | CommandStatus::Sent) if !ready => self.abandon(&id),
                Some(CommandStatus::Queued | CommandStatus::Sent) => {}
            }
        }
        if !ready {
            for request in std::mem::take(&mut self.held) {
                self.orphaned.push(request);
                self.outbox.push(TitleUpdate::Abandoned { request });
            }
        }
        for reply in self.bridge.poll(client.model()) {
            let request = reply.request();
            self.held.retain(|held| *held != request);
            if let Some(index) = self.orphaned.iter().position(|orphan| *orphan == request) {
                self.orphaned.remove(index);
            } else {
                self.outbox.push(TitleUpdate::Reply(reply));
            }
        }
        std::mem::take(&mut self.outbox)
    }
}
