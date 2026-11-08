//! Update an existing artifact by appending a new version.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tinytools::{PermissionLevel, Tool, ToolResult};

use crate::service::{Artifacts, Author, Content};
use crate::tools::{content_for, optional_str, read_required_str, saved_result, tool_error};

/// Append a new revision to an existing artifact.
pub struct UpdateArtifactTool {
    artifacts: Arc<Artifacts>,
    author: Author,
}

impl std::fmt::Debug for UpdateArtifactTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateArtifactTool").finish_non_exhaustive()
    }
}

impl UpdateArtifactTool {
    /// Constructs the tool bound to one service, writing versions as
    /// `author` (the agent the tool is built for).
    #[must_use]
    pub fn new(artifacts: Arc<Artifacts>, author: Author) -> Self {
        Self { artifacts, author }
    }

    /// Run the tool with `content` for the new version instead of the `body`
    /// or `path` in `args`: for a host that already holds the content (a file
    /// read out of a sandbox) and so has no path to give. `artifact_id` and
    /// `note` are still read from `args`, and the result is the one
    /// [`Tool::execute`] returns.
    ///
    /// # Errors
    ///
    /// As [`Tool::execute`], for a missing `artifact_id` or a store failure.
    pub async fn execute_with_content(
        &self,
        args: Value,
        content: Content,
    ) -> anyhow::Result<ToolResult> {
        self.run(&args, Some(content)).await
    }

    async fn run(&self, args: &Value, content: Option<Content>) -> anyhow::Result<ToolResult> {
        let id = read_required_str(args, "artifact_id")?;
        let record = match self.artifacts.get(&id).await {
            Ok(Some(record)) => record,
            Ok(None) => return Ok(ToolResult::error(format!("artifact `{id}` not found"))),
            Err(err) => return tool_error(err),
        };
        let content = match content {
            Some(content) => content,
            None => content_for(record.kind, args)?,
        };
        match self
            .artifacts
            .revise(&id, content, &self.author, optional_str(args, "note"))
            .await
        {
            Ok(saved) => Ok(saved_result(&saved)),
            Err(err) => tool_error(err),
        }
    }
}

#[async_trait]
impl Tool for UpdateArtifactTool {
    fn name(&self) -> &'static str {
        "update_artifact"
    }

    fn description(&self) -> &'static str {
        "Append a new revision to an existing artifact; earlier versions are \
         kept. Text artifacts take the full new `body`; image and file \
         artifacts take the new file's `path`. A body identical to the newest \
         version adds nothing. Use `read_artifact` first to see the history \
         (including the user's own edits). Returns the new version number."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "artifact_id": {
                    "type": "string",
                    "description": "The artifact id to update."
                },
                "body": {
                    "type": "string",
                    "description": "For text artifacts: the full new content."
                },
                "path": {
                    "type": "string",
                    "description": "For image and file artifacts: the new file, relative to your working folder."
                },
                "note": {
                    "type": "string",
                    "description": "Optional note about this revision (e.g. why it was changed)."
                }
            },
            "required": ["artifact_id"]
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        self.run(&args, None).await
    }

    fn is_concurrency_safe(&self, _args: &Value) -> bool {
        false
    }
}
