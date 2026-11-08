//! Create a new artifact with its first version.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tinytools::{PermissionLevel, Tool, ToolResult};

use crate::service::{Artifacts, Author, Content};
use crate::tools::{content_for, read_required_str, saved_result, tool_error};
use crate::types::ArtifactKind;

/// Create a new artifact with its first version. Use `update_artifact`
/// later to append revisions.
pub struct CreateArtifactTool {
    artifacts: Arc<Artifacts>,
    author: Author,
}

impl std::fmt::Debug for CreateArtifactTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateArtifactTool").finish_non_exhaustive()
    }
}

impl CreateArtifactTool {
    /// Constructs the tool bound to one service, writing versions as
    /// `author` (the agent the tool is built for).
    #[must_use]
    pub fn new(artifacts: Arc<Artifacts>, author: Author) -> Self {
        Self { artifacts, author }
    }

    /// Run the tool with `content` for the artifact's body instead of the
    /// `body` or `path` in `args`: for a host that already holds the content
    /// (a file read out of a sandbox) and so has no path to give. `kind` and
    /// `title` are still read from `args` and checked, and the result is the
    /// one [`Tool::execute`] returns.
    ///
    /// # Errors
    ///
    /// As [`Tool::execute`], for a missing or invalid `kind` or `title` or a
    /// store failure.
    pub async fn execute_with_content(
        &self,
        args: Value,
        content: Content,
    ) -> anyhow::Result<ToolResult> {
        self.run(&args, Some(content)).await
    }

    async fn run(&self, args: &Value, content: Option<Content>) -> anyhow::Result<ToolResult> {
        let kind_raw = read_required_str(args, "kind")?;
        let kind = ArtifactKind::parse(&kind_raw).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid kind `{kind_raw}`; expected one of html, markdown, text, image, file"
            )
        })?;
        let title = read_required_str(args, "title")?;
        let content = match content {
            Some(content) => content,
            None => content_for(kind, args)?,
        };
        match self
            .artifacts
            .create(kind, &title, content, &self.author, None)
            .await
        {
            Ok(saved) => Ok(saved_result(&saved)),
            Err(err) => tool_error(err),
        }
    }
}

#[async_trait]
impl Tool for CreateArtifactTool {
    fn name(&self) -> &'static str {
        "create_artifact"
    }

    fn description(&self) -> &'static str {
        "Create a deliverable the user can see and keep, with its first version. \
         Text kinds take `body`: `html` (an HTML page), `markdown` (a document), \
         `text` (plain text). File kinds take `path`, a file you already wrote \
         in your working folder: `image` (an image), `file` (any other file — a \
         PDF, a spreadsheet, an archive). Use this instead of only writing a \
         file: a file that is not an artifact never reaches the user. Returns \
         the artifact id — pass it to `update_artifact` for later revisions."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["html", "markdown", "text", "image", "file"],
                    "description": "Artifact kind."
                },
                "title": {
                    "type": "string",
                    "description": "A short display title for the artifact."
                },
                "body": {
                    "type": "string",
                    "description": "For `html`, `markdown` and `text`: the full content."
                },
                "path": {
                    "type": "string",
                    "description": "For `image` and `file`: the file to file, relative to your working folder or absolute inside it."
                }
            },
            "required": ["kind", "title"]
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
