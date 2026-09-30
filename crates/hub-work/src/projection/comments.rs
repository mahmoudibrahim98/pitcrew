//! `work.comments`: comments on tasks and workstreams, with their mentions.

use super::{clear, exec};
use crate::codec::{IdText, opt_text, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_store::sql::{Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Comments.
#[derive(Debug, Clone, Copy, Default)]
pub struct Comments;

impl Comments {
    /// The projection's name.
    pub const NAME: &'static str = "work.comments";
    const VERSION: u32 = 1;
}

impl Projection for Comments {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        clear(tx, &["work_comment_mentions", "work_comments"])
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        let EventBody::CommentPosted {
            task,
            workstream,
            text,
            mentions,
        } = &stored.event.body
        else {
            return Ok(());
        };
        let e = &stored.event;
        let id = e.id.text();
        exec(
            tx,
            "INSERT INTO work_comments (event, rev, at, author, on_behalf_of, task, workstream,
               text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                sql_rev(stored.rev),
                e.at,
                e.author.text(),
                opt_text(e.on_behalf_of.as_ref()),
                opt_text(task.as_ref()),
                opt_text(workstream.as_ref()),
                text,
            ],
        )?;
        for (position, member) in mentions.iter().enumerate() {
            exec(
                tx,
                "INSERT INTO work_comment_mentions (comment, position, member)
                 VALUES (?1, ?2, ?3)",
                params![id, i64::try_from(position)?, member.text()],
            )?;
        }
        Ok(())
    }
}
